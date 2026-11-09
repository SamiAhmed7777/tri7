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
         \x20                      [-min | --min]  start minimized (the login-autostart flag)\n\
         \x20                      --autostart-check  roundtrip the login registration (headless)\n\
         defaults: wallet /root/.triangles/wallet.dat, state-dir /var/lib/tri7,\n\
         \x20         rpc 127.0.0.1:19114"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selfcheck = args.iter().any(|a| a == "--selfcheck");
    let history_probe = args.iter().any(|a| a == "--history-probe");
    // v5 -min: start MINIMIZED (the start-on-login registration launches
    // with -min; guiutil.cpp passes SW_SHOWMINNOACTIVE/-min the same way)
    let start_minimized = args.iter().any(|a| a == "-min" || a == "--min");
    // --autostart-check: headless roundtrip of the start-on-login
    // registration (enable → read → disable → read; the OS entry is left
    // CLEAN). Explicit opt-in because it writes the user's Run key /
    // autostart dir — never run as a side effect of anything else.
    if args.iter().any(|a| a == "--autostart-check") {
        let supported = tri7_core::gui::autostart::supported();
        println!("autostart: supported={supported}");
        if !supported {
            eprintln!("autostart roundtrip: unsupported platform");
            std::process::exit(2);
        }
        let outcome = (|| -> Result<bool, String> {
            tri7_core::gui::autostart::set_enabled(true)?;
            let on = tri7_core::gui::autostart::is_enabled();
            tri7_core::gui::autostart::set_enabled(false)?;
            let off = tri7_core::gui::autostart::is_enabled();
            Ok(on && !off)
        })();
        match outcome {
            Ok(true) => {
                println!("autostart roundtrip: PASS (registered, read back, removed cleanly)");
                std::process::exit(0);
            }
            Ok(false) => {
                println!("autostart roundtrip: FAIL (readback mismatch)");
                std::process::exit(1);
            }
            Err(e) => {
                println!("autostart roundtrip: ERROR {e}");
                std::process::exit(1);
            }
        }
    }
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
        cfg.state_dir_explicit = true;
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
    if history_probe {
        // headless: construct the app state, load the wallet exactly like the
        // GUI does, then print the history the Transactions page would show
        let mut st = tri7_core::gui::app_state::AppState::new(cfg.clone());
        match st.load_wallet_action() {
            Ok(()) => {}
            Err(e) => {
                println!("load failed: {}", e);
                std::process::exit(1);
            }
        }
        st.refresh_debug_info_action();
        let (rows, note) = st.load_history_file();
        // what the Transactions table actually lists (history + live coins)
        let merged = st.merged_transaction_rows();
        let newest = merged.first().map(|r| (r.height, r.tx_time)).unwrap_or((0, 0));
        println!(
            "merged rows: {} | newest row h{} ({})",
            merged.len(),
            newest.0,
            tri7_core::gui::app_state::stamp_date(newest.1)
        );
        let oldest = rows.iter().map(|r| r.time).min().unwrap_or(0);
        let newest = rows.iter().map(|r| r.time).max().unwrap_or(0);
        println!("wallet: {}", cfg.wallet_path);
        println!("state dir: {}", cfg.state_dir);
        println!("history rows: {}", rows.len());
        println!("oldest: {}", tri7_core::gui::app_state::stamp_date(oldest));
        println!("newest: {}", tri7_core::gui::app_state::stamp_date(newest));
        println!("note: {}", note);
        // the Overview split, same function the page renders
        let (sp, sk, un, im, total, tip) = st.balance_split();
        println!(
            "split @ tip h{}: spendable {:.6} | stake {:.6} | unconfirmed {:.6} | immature {:.6} | total {:.6}",
            tip,
            sp as f64 / 1e6,
            sk as f64 / 1e6,
            un as f64 / 1e6,
            im as f64 / 1e6,
            total as f64 / 1e6
        );
        // sample rows EXACTLY as the page renders them: the shared row view
        // (page == CSV == probe), printing the rendered status sentence and
        // amount string verbatim, plus the hover tooltip it would show
        let (_, _, _, _, _, tip_h) = st.balance_split();
        let mut stake_printed = 0usize;
        let mut send_printed = 0usize;
        for (i, r) in merged.iter().enumerate() {
            let view = st.tx_row_view(r, tip_h as u32);
            let is_stake = r.prev_kind == "coinstake" || r.legacy_kind == "mined";
            let is_send = view.negative;
            if i >= 5 {
                if !(is_stake && stake_printed < 3) && !(is_send && send_printed < 3) {
                    continue;
                }
                if is_stake {
                    stake_printed += 1;
                } else if is_send {
                    send_printed += 1;
                }
            }
            println!(
                "row h{} depth {} -> {} | amt {} | txid {} | tooltip \"{}\"",
                r.height,
                tri7_core::legacy_rows::depth_of(r.height as u64, tip_h),
                view.status_text,
                view.amount_text,
                r.txid_display(),
                view.tooltip.replace('\n', " / ")
            );
        }
        println!("--- what the page would print ---");
        println!("{}", st.history_status_line());
        // --history-probe-full: audit dump — EVERY merged row through the
        // same shared view (the default samples 5 + 3 stake + 3 send)
        if args.iter().any(|a| a == "--history-probe-full") {
            println!("--- full dump: {} rows ---", merged.len());
            for r in merged.iter() {
                let view = st.tx_row_view(r, tip_h as u32);
                println!(
                    "row h{} depth {} -> {} | amt {} | txid {} | tooltip \"{}\"",
                    r.height,
                    tri7_core::legacy_rows::depth_of(r.height as u64, tip_h),
                    view.status_text,
                    view.amount_text,
                    r.txid_display(),
                    view.tooltip.replace('\n', " / ")
                );
            }
        }
        std::process::exit(if merged.is_empty() { 1 } else { 0 });
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
                        | "--min"
                )
        }) {
            eprintln!("unknown flag {}", bad);
            usage();
        }
    }

    #[cfg(feature = "gui")]
    {
        app::run(cfg, start_minimized);
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
