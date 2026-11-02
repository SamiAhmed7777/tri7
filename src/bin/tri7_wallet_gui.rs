//! tri7 GUI wallet binary (phase 6).
//!
//!   tri7_wallet_gui               launch the egui app (needs --features gui
//!                                 and a display; used on sami-pc/laptop)
//!   tri7_wallet_gui --selfcheck   headless validation: construct app state,
//!                                 load the real wallet, derive balances,
//!                                 build one signed tx, exit 0. No display,
//!                                 no broadcast, wallet/archive read-only.
//!
//! Exit codes: 0 selfcheck OK / 1 selfcheck failed / 2 usage or no-gui build.

#[cfg(feature = "gui")]
use tri7_core::gui::app;

fn usage() -> ! {
    eprintln!(
        "usage: tri7_wallet_gui [--selfcheck] [--report PATH]\n\
         \x20                      [--wallet PATH] [--state-dir PATH] [--dump PATH]\n\
         \x20                      [--tipstate PATH] [--archive PATH] [--index-dir PATH]\n\
         \x20                      [--stake-dryrun PATH] [--rpc ADDR|none] [--profile-dir PATH]\n\
         defaults: wallet /root/.triangles/wallet.dat, state-dir /var/lib/tri7,\n\
         \x20         rpc 127.0.0.1:19114"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selfcheck = args.iter().any(|a| a == "--selfcheck");
    let mut cfg = tri7_core::gui::state::UiConfig::default();
    let mut report: Option<String> = None;
    let get = |flag: &str, args: &[String]| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    for (flag, setter) in [
        ("--wallet", 0u8),
        ("--state-dir", 1),
        ("--stake-dryrun", 2),
        ("--profile-dir", 3),
    ] {
        if args.iter().any(|a| a == flag) && get(flag, &args).is_none() {
            usage();
        }
        let _ = setter;
    }
    if let Some(v) = get("--wallet", &args) {
        cfg.wallet_path = v;
    }
    if let Some(v) = get("--state-dir", &args) {
        cfg.state_dir = v;
    }
    if let Some(v) = get("--stake-dryrun", &args) {
        cfg.stake_dryrun_path = v;
    }
    if let Some(v) = get("--profile-dir", &args) {
        cfg.profile_dir = v;
    }
    if let Some(v) = get("--rpc", &args) {
        cfg.rpc_addr = if v == "none" { None } else { Some(v) };
    }
    if selfcheck {
        report = get("--report", &args);
        std::process::exit(tri7_core::gui::app_state::run_selfcheck(
            &cfg,
            report.as_deref(),
        ));
    }
    if !selfcheck {
        // any leftover unknown flag → usage
        if let Some(bad) = args.iter().find(|a| {
            a.starts_with("--")
                && !matches!(
                    a.as_str(),
                    "--selfcheck"
                        | "--report"
                        | "--wallet"
                        | "--state-dir"
                        | "--dump"
                        | "--tipstate"
                        | "--archive"
                        | "--index-dir"
                        | "--stake-dryrun"
                        | "--rpc"
                        | "--profile-dir"
                )
        }) {
            eprintln!("unknown flag {}", bad);
            usage();
        }
    }

    #[cfg(feature = "gui")]
    {
        app::run(cfg);
    }
    #[cfg(not(feature = "gui"))]
    {
        eprintln!(
            "tri7_wallet_gui: built WITHOUT the `gui` feature — no display layer.\n\
             rebuild with: cargo build --release --features gui\n\
             (headless validation: tri7_wallet_gui --selfcheck)"
        );
        std::process::exit(2);
    }
    let _ = report;
}
