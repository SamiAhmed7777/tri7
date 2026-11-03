//! wallet_keytool — legacy rpcdump.cpp parity CLI (parity-audit 2026-11-03).
//!
//! The dumpwallet/importwallet/dumpprivkey/checkwallet capability the legacy
//! trianglesd exposed over RPC, as a standalone tool over tri7's SQLite
//! wallet. Logic lives in tri7_core::keytool (this bin is a thin argv shell
//! so the lib carries the tested implementation).
//!
//! Usage:
//!   wallet_keytool dumpprivkey <wallet.dat> <address>
//!   wallet_keytool dumpwallet   <wallet.dat> <out.txt> [tip_height tip_hash]
//!   wallet_keytool importwallet <wallet.dat> <dump.txt>
//!   wallet_keytool checkwallet  <wallet.dat>
//!
//! Exit codes: 0 ok, 1 refused/failed, 2 usage.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || {
        eprintln!(
            "usage:\n  wallet_keytool dumpprivkey <wallet.dat> <address>\n  wallet_keytool dumpwallet <wallet.dat> <out.txt> [tip_height tip_hash]\n  wallet_keytool importwallet <wallet.dat> <dump.txt>\n  wallet_keytool checkwallet <wallet.dat>"
        );
    };
    if args.is_empty() {
        usage();
        return ExitCode::from(2);
    }
    match (args[0].as_str(), args.len()) {
        ("dumpprivkey", 3) => match tri7_core::keytool::dump_privkey(&p(&args[1]), &args[2]) {
            Ok(wif) => {
                println!("{}", wif);
                ExitCode::from(0)
            }
            Err(e) => {
                eprintln!("error: {}", e);
                ExitCode::from(1)
            }
        },
        ("dumpwallet", 3) | ("dumpwallet", 5) => {
            let best = if args.len() == 5 {
                match (args[3].parse::<u64>(), args[4].is_empty()) {
                    (Ok(h), false) => Some((h, args[4].as_str())),
                    _ => {
                        eprintln!("error: tip_height must be an integer");
                        return ExitCode::from(2);
                    }
                }
            } else {
                None
            };
            match tri7_core::keytool::dump_wallet(&p(&args[1])) {
                Ok((rows, n)) => {
                    let out = std::fs::File::create(&args[2]);
                    match out.and_then(|mut f| tri7_core::keytool::write_dump(&mut f, &rows, best)) {
                        Ok(()) => {
                            println!("dumped {} key(s) to {}", n, args[2]);
                            ExitCode::from(0)
                        }
                        Err(e) => {
                            eprintln!("error: write {}: {}", args[2], e);
                            ExitCode::from(1)
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }
        ("importwallet", 3) => match tri7_core::keytool::import_wallet(&p(&args[1]), &p(&args[2])) {
            Ok(rep) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "imported": rep.imported,
                        "skipped": rep.skipped,
                        "failed": rep.failed,
                        "errors": rep.errors,
                    })
                );
                if rep.failed == 0 {
                    ExitCode::from(0)
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => {
                eprintln!("error: {}", e);
                ExitCode::from(1)
            }
        },
        ("checkwallet", 2) => match tri7_core::keytool::check_wallet(&p(&args[1])) {
            Ok(rep) => {
                println!("{}", rep.to_json());
                let ok = rep.integrity == "ok" && rep.bad_keys == 0;
                ExitCode::from(if ok { 0 } else { 1 })
            }
            Err(e) => {
                eprintln!("error: {}", e);
                ExitCode::from(1)
            }
        },
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}

fn p(s: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(s)
}
