//! egui view layer — the legacy triangles-qt identity on the tri7 engine.
//!
//! Everything visual here is carried over from triangles_v5/src/qt (v6.2.6.7,
//! the current legacy wallet):
//!   - palette: forms/mainwindow.ui stylesheet (#000 window, #1c1c1c inputs,
//!     #e32105 accent, #3d0e04 hover/selection, #AB7F82 header, menus
//!     #AF272E/#9D0B17/#deeef1)
//!   - icons: res/icons/menu_16/*, triangles.png, staking_off, lock_open,
//!     connect0/4, synced/notsynced, tx/clock icons — the actual legacy PNGs,
//!     copied into src/gui/assets/ and embedded via include_bytes!
//!   - branding: header_logo.png ("Cryptographic Triangles"), about.png,
//!     splash.png
//!   - page set + labels: Overview / Send Coins / Receive Coins / Address
//!     Book / Transactions / Messages (+ Staking, Sign/Verify Message,
//!     Options), Debug window + About dialogs — exact sidebar order and the
//!     .ui <string> labels.
//!
//! All behavior comes from gui::app_state::AppState (pure, tested headless);
//! this file only renders it and forwards clicks to its action methods.

use crate::consensus::STAKE_MIN_AGE;
use crate::gui::app_state::AppState;
use crate::gui::explorer as ex;
use crate::gui::state as s;
use crate::gui::state::{theme as t, Screen, UiConfig, UtxoRec};
use eframe::egui;
use egui::{Color32, TextureHandle, TextureOptions, Vec2};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

const fn c(rgb: t::Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

const RED_ERR: Color32 = Color32::from_rgb(0xff, 0x55, 0x44);
const OK_GRN: Color32 = Color32::from_rgb(0x55, 0xcc, 0x66);

/// Size a texture into an egui Image (egui 0.29 SizedTexture path).
fn img(tex: &TextureHandle, size: Vec2) -> egui::Image<'static> {
    egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), size))
}

// ---------------------------------------------------------------------------
// Assets (PNGs copied from triangles_v5/src/qt/res — the actual legacy files)
// ---------------------------------------------------------------------------

macro_rules! asset_map {
    ($($key:literal => $path:literal),* $(,)?) => {
        fn asset_bytes(key: &str) -> Option<&'static [u8]> {
            match key {
                $($key => Some(include_bytes!(concat!("assets/", $path))),)*
                _ => None,
            }
        }
    };
}

asset_map! {
    "menu_16/overview" => "menu_16/overview.png",
    "menu_16/send" => "menu_16/send.png",
    "menu_16/receive" => "menu_16/receive.png",
    "menu_16/addressbook" => "menu_16/addressbook.png",
    "menu_16/transactions" => "menu_16/transactions.png",
    "menu_16/messages" => "menu_16/messages.png",
    "menu_16/options" => "menu_16/options.png",
    "menu_16/debug" => "menu_16/debug.png",
    "menu_16/passphrase" => "menu_16/passphrase.png",
    "menu_16/lock" => "menu_16/lock.png",
    "menu_16/unlock" => "menu_16/unlock.png",
    "menu_16/sign" => "menu_16/sign.png",
    "menu_16/verify" => "menu_16/verify.png",
    "menu_16/copy" => "menu_16/copy.png",
    "menu_16/edit" => "menu_16/edit.png",
    "menu_16/export" => "menu_16/export.png",
    "menu_16/backup" => "menu_16/backup.png",
    "menu_16/exit" => "menu_16/exit.png",
    "icons/triangles" => "icons/triangles.png",
    "icons/triangles-128" => "icons/triangles-128.png",
    "icons/staking_off" => "icons/staking_off.png",
    "icons/staking_on" => "icons/staking_on.png",
    "icons/lock_open" => "icons/lock_open.png",
    "icons/synced" => "icons/synced.png",
    "icons/notsynced" => "icons/notsynced.png",
    "icons/connect0" => "icons/connect0_16.png",
    "icons/connect4" => "icons/connect4_16.png",
    "icons/tx_confirmed" => "icons/transaction2.png",
    "icons/clock1" => "icons/clock1.png",
    "images/header_logo" => "images/header_logo.png",
    "images/about" => "images/about.png",
    "images/splash" => "images/splash.png",
}

fn decode_rgba(bytes: &'static [u8]) -> egui::ImageData {
    let img = image::load_from_memory(bytes).expect("embedded legacy asset must decode");
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    egui::ImageData::Color(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [w, h],
        rgba.as_raw(),
    )))
}

fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(asset_bytes("icons/triangles-128").unwrap())
        .expect("embedded icon")
        .to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// UTC "YYYY-MM-DD HH:MM:SS" from unix seconds (Howard Hinnant
/// civil-from-days; no chrono dependency).
fn fmt_unix(secs: u32) -> String {
    let secs = secs as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mth, d, h, mi, sec)
}

fn recent_utxos(st: &AppState, n: usize) -> Vec<UtxoRec> {
    let Some(d) = &st.derived else {
        return Vec::new();
    };
    let mut v = d.utxos.clone();
    v.sort_by(|a, b| b.height.cmp(&a.height).then(b.tx_time.cmp(&a.tx_time)));
    v.truncate(n);
    v
}

fn now_u32() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(1_400_000_000)
}

static DEBUG_TOGGLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct Tri7App {
    st: AppState,
    tex: HashMap<String, TextureHandle>,
    console_open: bool,
}

impl Tri7App {
    fn tex(&mut self, ctx: &egui::Context, key: &str) -> Option<TextureHandle> {
        if let Some(x) = self.tex.get(key) {
            return Some(x.clone());
        }
        let bytes = asset_bytes(key)?;
        let handle = ctx.load_texture(
            format!("asset/{}", key),
            decode_rgba(bytes),
            TextureOptions::LINEAR,
        );
        self.tex.insert(key.to_string(), handle.clone());
        Some(handle)
    }

    fn ico(&mut self, ui: &mut egui::Ui, key: &str, size: f32) {
        if let Some(tex) = self.tex(ui.ctx(), key) {
            ui.add(img(&tex, Vec2::splat(size)));
        }
    }

    fn apply_theme(ctx: &egui::Context) {
        ctx.set_visuals(egui::Visuals::dark());
        let mut style = (*ctx.style()).clone();
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(c(t::ACCENT)); // QWidget { color: #e32105 }
        v.panel_fill = c(t::WINDOW_BG); // #MainWindow { background-color: #000 }
        v.window_fill = c(t::WINDOW_BG);
        v.extreme_bg_color = c(t::PANEL_BG); // QComboBox/QLineEdit bg #1c1c1c
        v.faint_bg_color = c(t::PANEL_BG);
        v.selection.bg_fill = c(t::DEEP_RED); // hover/selected #3d0e04
        v.selection.stroke = egui::Stroke::new(1.0, c(t::ACCENT));
        v.hyperlink_color = c(t::ACCENT);
        v.window_stroke = egui::Stroke::new(2.0, c(t::ACCENT)); // centralWidget 2px border
        v.window_shadow = egui::Shadow::NONE;
        let w = &mut v.widgets;
        w.noninteractive.fg_stroke = egui::Stroke::new(1.0, c(t::ACCENT));
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0, c(t::ACCENT));
        w.noninteractive.bg_fill = c(t::WINDOW_BG);
        w.inactive.fg_stroke = egui::Stroke::new(1.0, c(t::ACCENT));
        w.inactive.bg_fill = c(t::WINDOW_BG);
        w.hovered.fg_stroke = egui::Stroke::new(1.0, c(t::ACCENT));
        w.hovered.bg_fill = c(t::DEEP_RED); // QPushButton:hover #3d0e04
        w.active.fg_stroke = egui::Stroke::new(1.0, c(t::WINDOW_BG));
        w.active.bg_fill = c(t::ACCENT); // pressed:flat #e32105 (black text)
        style.spacing.item_spacing = Vec2::new(8.0, 5.0);
        ctx.set_style(style);
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("wHeader").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tex) = self.tex(ui.ctx(), "images/header_logo") {
                    ui.add(img(&tex, Vec2::new(150.0, 49.0)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("Help", |ui| {
                        if ui.button("&Debug window").clicked() {
                            DEBUG_TOGGLE.store(true, std::sync::atomic::Ordering::Relaxed);
                            ui.close_menu();
                        }
                        if ui.button("&About Triangles").clicked() {
                            self.st.dialogs.about = true;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("Operations", |ui| {
                        for sc in [
                            Screen::Send,
                            Screen::Receive,
                            Screen::AddressBook,
                            Screen::Transactions,
                            Screen::Messages,
                            Screen::Staking,
                        ] {
                            if ui.button(sc.label()).clicked() {
                                self.st.goto(sc);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("File", |ui| {
                        if ui.button("&Encrypt Wallet...").clicked() {
                            self.st.encrypt_wallet_stub_action();
                            ui.close_menu();
                        }
                        if ui.button("&Backup Wallet...").clicked() {
                            self.st.goto(Screen::Overview);
                            ui.close_menu();
                        }
                        if ui.button("Sign &message...").clicked() {
                            self.st.goto(Screen::SignMessage);
                            ui.close_menu();
                        }
                        if ui.button("&Verify message...").clicked() {
                            self.st.goto(Screen::VerifyMessage);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("E&xit Triangles wallet").clicked() {
                            std::process::exit(0);
                        }
                    });
                });
            });
        });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, screen: Screen) {
        let icon = format!("menu_16/{}", screen.icon());
        let selected = self.st.screen == screen;
        let fill = if selected {
            c(t::DEEP_RED)
        } else {
            Color32::TRANSPARENT
        };
        ui.horizontal(|ui| {
            self.ico(ui, &icon, 16.0);
            let btn = egui::Button::new(egui::RichText::new(screen.label()))
                .fill(fill)
                .min_size(Vec2::new(150.0, 28.0));
            if ui.add_sized([150.0, 28.0], btn).clicked() {
                self.st.goto(screen);
            }
        });
    }

    fn banner_no_broadcast(ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(
            "BROADCAST DISABLED in this build — signing is real (spend.rs, offline); the signed hex is produced for the operator. Network sending arrives with daemon integration.",
        )
        .small());
    }
}

impl eframe::App for Tri7App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Self::apply_theme(ctx);
        self.header(ctx);

        egui::SidePanel::left("nav")
            .exact_width(190.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                for sc in Screen::NAV_ORDER {
                    self.nav_button(ui, sc);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        self.ico(ui, "icons/lock_open", 16.0);
                        ui.label(egui::RichText::new("unencrypted").small());
                    });
                    ui.horizontal(|ui| {
                        self.ico(ui, "icons/staking_off", 16.0);
                        ui.label(egui::RichText::new("staking off").small());
                    });
                });
            });

        egui::TopBottomPanel::bottom("wStatusBar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let conn_icon = if self
                    .st
                    .debug_info
                    .as_ref()
                    .map(|d| d.reachable)
                    .unwrap_or(false)
                {
                    "icons/connect4"
                } else {
                    "icons/connect0"
                };
                self.ico(ui, conn_icon, 16.0);
                let conns = self
                    .st
                    .debug_info
                    .as_ref()
                    .and_then(|d| d.connections)
                    .map(|x| format!("{} connections", x))
                    .unwrap_or_else(|| "0 connections".into());
                ui.label(egui::RichText::new(conns).small());
                ui.separator();
                self.ico(
                    ui,
                    if self.st.tip.is_some() {
                        "icons/synced"
                    } else {
                        "icons/notsynced"
                    },
                    16.0,
                );
                ui.label(egui::RichText::new("Synchronization").small());
                let (progress, label) = self.sync_progress();
                ui.add(
                    egui::ProgressBar::new(progress)
                        .show_percentage()
                        .desired_height(14.0),
                );
                ui.label(egui::RichText::new(label).small());
                ui.separator();
                self.ico(ui, "icons/staking_off", 16.0);
                ui.label(egui::RichText::new("Staking: dry-run only").small());
                if ui
                    .add(egui::Button::new(egui::RichText::new("refresh").small()))
                    .clicked()
                {
                    self.st.refresh_debug_info_action();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.st.loaded() {
                egui::ScrollArea::vertical().show(ui, |ui| match self.st.screen {
                    Screen::Overview => self.page_overview(ui),
                    Screen::Send => self.page_send(ui),
                    Screen::Receive => self.page_receive(ui),
                    Screen::AddressBook => self.page_addressbook(ui),
                    Screen::Transactions => self.page_transactions(ui),
                    Screen::Staking => self.page_staking(ui),
                    Screen::Messages => self.page_messages(ui),
                    Screen::SignMessage => self.page_sign(ui),
                    Screen::VerifyMessage => self.page_verify(ui),
                    Screen::Options => self.page_options(ui),
                    Screen::Explorer => self.page_explorer(ui),
                });
            } else {
                self.load_screen(ui);
            }
        });

        self.dialogs(ctx);
        self.debug_window(ctx);
        if DEBUG_TOGGLE.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.console_open = true;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------------------
// Panels / pages
// ---------------------------------------------------------------------------

impl Tri7App {
    fn sync_progress(&self) -> (f32, String) {
        match (&self.st.tip, &self.st.debug_info) {
            (Some(tip), Some(di)) if di.reachable && di.blocks == Some(tip.height) => {
                (1.0, self.st.status_blocks_label())
            }
            (Some(tip), Some(di)) if di.reachable => {
                let d = di.blocks.unwrap_or(0);
                let p = if d > 0 {
                    tip.height as f32 / d.max(1) as f32
                } else {
                    0.0
                };
                (p.clamp(0.0, 1.0), self.st.status_blocks_label())
            }
            (Some(tip), _) => (1.0, format!("{} blocks (no tri7d RPC)", tip.height)),
            _ => (0.0, "no chain state".into()),
        }
    }

    fn load_screen(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(30.0);
            if let Some(tex) = self.tex(ui.ctx(), "images/splash") {
                ui.add(img(&tex, Vec2::new(400.0, 225.0)));
            }
            ui.add_space(10.0);
            ui.heading(egui::RichText::new(s::APP_TITLE).size(24.0));
            ui.label("tri7 engine — wallet.dat opened read-only; nothing is broadcast in this build");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("Wallet file:");
                let resp = ui.add_sized(
                    [420.0, 24.0],
                    egui::TextEdit::singleline(&mut self.st.cfg.wallet_path),
                );
                if resp.changed() {
                    self.st.load_error = None;
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("Open Wallet").size(16.0)))
                    .clicked()
                {
                    if let Err(e) = self.st.load_wallet_action() {
                        self.st.load_error = Some(e);
                    } else {
                        self.st.refresh_debug_info_action();
                    }
                }
            });
            if let Some(e) = &self.st.load_error {
                ui.colored_label(RED_ERR, e);
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new(
                "key count + coverage height appear after load; default wallet: /root/.triangles/wallet.dat",
            )
            .small());
        });
    }

    fn page_overview(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Overview");
        ui.columns(2, |cols| {
            // ---- Wallet frame (overviewpage.ui: exact labels + tooltips) ----
            let d = &mut cols[0];
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, c(t::ACCENT)))
                .inner_margin(12.0)
                .show(d, |ui| {
                    ui.heading("Wallet");
                    let Some(derived) = st.derived.clone() else { return };
                    let total = derived.balances.total_sats;
                    let tip = st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
                    let mut immature = 0i64;
                    for u in &derived.utxos {
                        if !u.spendable_at(tip + 1) {
                            immature += u.value_sats;
                        }
                    }
                    let (spendable, unconfirmed, stake) = (total - immature, 0i64, 0i64);
                    let row = |ui: &mut egui::Ui, k: &str, tip: &str, v: &str| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(k).strong());
                            ui.label(v).on_hover_text(tip);
                        });
                    };
                    row(ui, "Spendable:", "Your current spendable balance", &s::format_tri(spendable));
                    row(ui, "Stake:", "Total of coins that was staked, and do not yet count toward the current balance", &s::format_tri(stake));
                    row(ui, "Unconfirmed:", "Total of transactions that have yet to be confirmed, and do not yet count toward the current balance", &s::format_tri(unconfirmed));
                    row(ui, "Immature:", "Mined balance that has not yet matured", &s::format_tri(immature));
                    ui.separator();
                    row(ui, "Total:", "Your current total balance", &s::format_tri(total));
                    ui.label(egui::RichText::new(format!(
                        "{} {} ({} sats) in {} UTXOs across {} addresses — coverage h{}{}",
                        total as f64 / 1_000_000.0,
                        s::TICKER,
                        total,
                        derived.utxos.len(),
                        derived.balances.per_address.iter().filter(|a| a.sats > 0).count(),
                        derived.coverage_height,
                        if derived.reconcile.enabled { ", live-tail reconciled" } else { ", snapshot" }
                    ))
                    .small());
                    if !derived.reconcile.note.is_empty() {
                        ui.label(egui::RichText::new(&derived.reconcile.note).small());
                    }
                });
            // ---- Recent transactions ----
            let r = &mut cols[1];
            egui::Frame::none()
                .stroke(egui::Stroke::new(1.0, c(t::ACCENT)))
                .inner_margin(12.0)
                .show(r, |ui| {
                    ui.label(egui::RichText::new("Recent transactions").strong());
                    ui.separator();
                    let rows = recent_utxos(st, 8);
                    egui::Grid::new("recent_tx").num_columns(4).striped(true).show(ui, |ui| {
                        for u in rows {
                            ui.label(egui::RichText::new(fmt_unix(u.tx_time)).small());
                            ui.label(egui::RichText::new("Received with").small());
                            ui.label(egui::RichText::new(&u.address).small());
                            ui.label(egui::RichText::new(format!("+{} {}", s::format_tri(u.value_sats), s::TICKER)).small());
                            ui.end_row();
                        }
                    });
                });
        });
        if let Some(backup) = &self.st.dialogs.backup {
            match backup {
                Ok(m) => {
                    ui.label(m.clone());
                }
                Err(e) => {
                    ui.colored_label(RED_ERR, e);
                }
            }
        }
        ui.separator();
        // Backup Wallet (File menu entry lands here — wallet.dat is copied,
        // never modified)
        ui.horizontal(|ui| {
            ui.label("Backup Wallet to:");
            let path = &mut self.st.dialogs.backup_path;
            ui.add_sized([380.0, 22.0], egui::TextEdit::singleline(path));
            if ui.button("Backup").clicked() && !self.st.dialogs.backup_path.is_empty() {
                let p = self.st.dialogs.backup_path.clone();
                match self.st.backup_wallet_action(&p) {
                    Ok(n) => {
                        self.st.dialogs.backup =
                            Some(Ok(format!("wallet backed up to {} ({} bytes)", p, n)))
                    }
                    Err(e) => self.st.dialogs.backup = Some(Err(e)),
                }
            }
            if let Some(tex) = self.tex(ui.ctx(), "menu_16/backup") {
                ui.add(img(&tex, Vec2::splat(16.0)));
            }
        });
    }

    fn page_send(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Send Coins");
        ui.label("Send Triangles");
        ui.separator();
        if let Some(derived) = &st.derived {
            let bal = derived
                .balances
                .per_address
                .iter()
                .find(|a| a.address == st.send.from)
                .map(|a| a.sats)
                .unwrap_or(0);
            ui.label(format!("Balance: {} {}", s::format_tri(bal), s::TICKER));
        }
        Self::banner_no_broadcast(ui);
        egui::Grid::new("send_grid").num_columns(2).show(ui, |ui| {
            ui.label("Send from:");
            let funded = st.funded_addresses();
            let from = &mut st.send.from;
            egui::ComboBox::from_id_source("from_addr")
                .selected_text(if from.is_empty() {
                    "select a funded address".into()
                } else {
                    format!(
                        "{} — {} {}",
                        from,
                        s::format_tri(
                            funded
                                .iter()
                                .find(|(a, _)| a == from)
                                .map(|(_, v)| *v)
                                .unwrap_or(0)
                        ),
                        s::TICKER
                    )
                })
                .width(420.0)
                .show_ui(ui, |ui| {
                    for (addr, bal) in &funded {
                        ui.selectable_value(
                            from,
                            addr.clone(),
                            format!("{} — {} {}", addr, s::format_tri(*bal), s::TICKER),
                        );
                    }
                });
            ui.end_row();
            ui.label("Send to:");
            let dest = &mut st.send.dest;
            ui.add_sized([480.0, 22.0], egui::TextEdit::singleline(dest));
            ui.end_row();
            ui.label("A&mount:");
            ui.horizontal(|ui| {
                let amt = &mut st.send.amount;
                ui.add_sized([180.0, 22.0], egui::TextEdit::singleline(amt));
                ui.label(format!("{} (6 decimals)", s::TICKER));
            });
            ui.end_row();
            ui.label("Fee:");
            ui.horizontal(|ui| {
                let fee = &mut st.send.fee;
                ui.add_sized([180.0, 22.0], egui::TextEdit::singleline(fee));
                ui.label("(auto-raised to the legacy GetMinFee minimum)");
            });
            ui.end_row();
            ui.label("Change:");
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.send.use_custom_change, "custom change address");
                if st.send.use_custom_change {
                    let cc = &mut st.send.custom_change;
                    ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(cc));
                }
            });
            ui.end_row();
        });

        // Coin Control Features (coincontroldialog carry-over)
        if st.options.coin_control {
            egui::CollapsingHeader::new("Coin Control Features")
                .default_open(false)
                .show(ui, |ui| {
                    let Some(derived) = st.derived.clone() else {
                        return;
                    };
                    ui.label(format!(
                        "Inputs... {}",
                        if st.send.selected.is_empty() {
                            "automatically selected"
                        } else {
                            "manually selected"
                        }
                    ));
                    let mut qty = 0usize;
                    let mut amt = 0i64;
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            egui::Grid::new("cc_grid")
                                .num_columns(5)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (i, u) in derived.utxos.iter().enumerate() {
                                        if u.address != st.send.from {
                                            continue;
                                        }
                                        let mut on = st.send.selected.contains(&i);
                                        if ui.checkbox(&mut on, "").changed() {
                                            if on {
                                                st.send.selected.push(i);
                                                st.send.selected.sort_unstable();
                                            } else {
                                                st.send.selected.retain(|x| *x != i);
                                            }
                                        }
                                        qty += 1;
                                        amt += u.value_sats;
                                        ui.label(
                                            u.txid_display().chars().take(18).collect::<String>(),
                                        );
                                        ui.label(&u.address);
                                        ui.label(s::format_tri(u.value_sats));
                                        ui.label(format!("h{}", u.height));
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.label(format!(
                        "Quantity: {}   Amount: {} {}",
                        qty,
                        s::format_tri(amt),
                        s::TICKER
                    ));
                    if ui.button("Clear &All").clicked() {
                        st.send.selected.clear();
                    }
                });
        }

        ui.separator();
        // The legacy button is "S&end"; this build NEVER broadcasts — signing
        // is real and the hex is handed to the operator.
        if ui
            .add(egui::Button::new(
                egui::RichText::new("S&ign — build transaction").size(16.0),
            ))
            .clicked()
        {
            st.build_send_action(now_u32());
        }
        let result = st.send_result.clone();
        match result {
            Some(Ok(b)) => {
                ui.colored_label(
                    c(t::MENU_SEL_TEXT),
                    format!(
                        "signed OK — txid {} — {} bytes — fee {} {} — change {} {} — inputs {}",
                        b.txid_display,
                        b.n_bytes,
                        s::format_tri(b.fee_sats),
                        s::TICKER,
                        s::format_tri(b.change_sats),
                        s::TICKER,
                        b.inputs.len()
                    ),
                );
                ui.horizontal(|ui| {
                    if ui.button("Copy hex to clipboard").clicked() {
                        ui.output_mut(|o| o.copied_text = b.tx_hex.clone());
                    }
                    ui.label("Save to file:");
                    let path = &mut st.dialogs.save_tx_path;
                    ui.add_sized([380.0, 22.0], egui::TextEdit::singleline(path));
                    if ui.button("Save").clicked() && !st.dialogs.save_tx_path.is_empty() {
                        let p = st.dialogs.save_tx_path.clone();
                        match st.save_tx_hex_action(&p) {
                            Ok(n) => st
                                .status_log
                                .push(format!("saved signed tx to {} ({} bytes)", p, n)),
                            Err(e) => st.status_log.push(format!("save failed: {}", e)),
                        }
                    }
                });
                ui.add(egui::Label::new(egui::RichText::new(&b.tx_hex).monospace().small()).wrap());
            }
            Some(Err(e)) => {
                ui.colored_label(RED_ERR, e);
            }
            None => {}
        }
        for line in st.status_log.iter().rev().take(3) {
            ui.label(egui::RichText::new(line).small());
        }
    }

    fn page_receive(&mut self, ui: &mut egui::Ui) {
        let copy_icon = self.tex(ui.ctx(), "menu_16/copy");
        let st = &mut self.st;
        ui.heading("Receive Coins");
        ui.label("Show the list of addresses for receiving payments");
        ui.separator();
        let addrs = st.receive_addresses();
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                egui::Grid::new("recv_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (addr, label, source) in &addrs {
                            let selected = st.receive.address == *addr;
                            if ui.selectable_label(selected, addr.clone()).clicked() {
                                st.receive.address = addr.clone();
                                st.refresh_receive_qr();
                            }
                            let lab = st
                                .label_of(addr)
                                .or_else(|| label.clone())
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}{}",
                                    lab,
                                    if *source == "keystore" {
                                        "  (GUI keystore)"
                                    } else {
                                        ""
                                    }
                                ))
                                .small(),
                            );
                            ui.end_row();
                        }
                    });
            });
        ui.horizontal(|ui| {
            ui.label("New Address label:");
            let nl = &mut st.receive.new_address_label;
            ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(nl));
            if ui.button("&New Address").clicked() {
                let label = st.receive.new_address_label.clone();
                match st.new_address_action(label) {
                    Ok(addr) => {
                        st.receive.address = addr;
                        st.refresh_receive_qr();
                    }
                    Err(e) => st.receive.error = Some(e),
                }
            }
            if ui.button("&Copy Address").clicked() {
                ui.output_mut(|o| o.copied_text = st.receive.address.clone());
            }
            if let Some(tex) = &copy_icon {
                ui.add(img(tex, Vec2::splat(16.0)));
            }
        });
        ui.separator();
        // Request Payment (qrcodedialog.ui replica): Label/Message/Amount + QR
        egui::Grid::new("qrform").num_columns(2).show(ui, |ui| {
            ui.label("Label:");
            let l = &mut st.receive.label;
            ui.text_edit_singleline(l);
            ui.end_row();
            ui.label("Message:");
            let m = &mut st.receive.message;
            ui.text_edit_singleline(m);
            ui.end_row();
            ui.label("Amount:");
            ui.horizontal(|ui| {
                let amt = &mut st.receive.amount;
                ui.add_enabled(
                    st.receive.require_payment,
                    egui::TextEdit::singleline(amt).desired_width(160.0),
                );
                ui.checkbox(&mut st.receive.require_payment, "request payment");
            });
            ui.end_row();
        });
        if ui.button("Show &QR Code").clicked() {
            st.refresh_receive_qr();
            st.dialogs.qr = true;
        }
        if let Some(err) = &st.receive.error {
            ui.colored_label(RED_ERR, err);
        }
        if !st.receive.uri.is_empty() {
            ui.label(format!("URI: {}", st.receive.uri));
        }
    }

    fn page_addressbook(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Address Book");
        ui.label("Double-click to edit address or label");
        ui.separator();
        let rows = st.all_book_rows();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("ab_grid")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    for (addr, label, src) in &rows {
                        ui.label(addr.clone());
                        let mut lb = label.clone().unwrap_or_default();
                        let editable = *src != "wallet";
                        let changed = ui
                            .add_enabled(
                                editable,
                                egui::TextEdit::singleline(&mut lb).desired_width(280.0),
                            )
                            .changed();
                        if editable && changed {
                            let a2 = addr.clone();
                            if let Err(e) = st.set_label_action(&a2, lb) {
                                st.status_log.push(format!("label: {}", e));
                            }
                        }
                        ui.label(
                            egui::RichText::new(match *src {
                                "wallet" => "wallet.dat",
                                "keystore" => "GUI keystore",
                                _ => "address book",
                            })
                            .small(),
                        );
                        if editable {
                            if ui.button("&Delete").clicked() {
                                let a2 = addr.clone();
                                if let Err(e) = st.delete_ab_entry_action(&a2) {
                                    st.status_log.push(format!("delete: {}", e));
                                }
                            }
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
        });
        ui.horizontal(|ui| {
            if ui.button("&Copy Address").clicked() {
                let first = rows.first().map(|(a, _, _)| a.clone()).unwrap_or_default();
                ui.output_mut(|o| o.copied_text = first);
            }
            if ui.button("Sign &Message").clicked() {
                st.goto(Screen::SignMessage);
            }
            if ui.button("&Verify Message").clicked() {
                st.goto(Screen::VerifyMessage);
            }
        });
    }

    fn page_transactions(&mut self, ui: &mut egui::Ui) {
        // prefetch icons (avoid &mut self while the state borrow is alive)
        let confirmed = self.tex(ui.ctx(), "icons/tx_confirmed");
        let pending = self.tex(ui.ctx(), "icons/clock1");
        let st = &mut self.st;
        ui.heading("Transactions");
        ui.separator();
        let Some(derived) = st.derived.clone() else {
            return;
        };
        let tip = st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
        ui.label(egui::RichText::new(
            "wallet outputs currently unspent (credits) — full sent/history view lands with daemon integration",
        )
        .small());
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("tx_grid")
                .num_columns(5)
                .striped(true)
                .show(ui, |ui| {
                    for u in derived.utxos.iter().rev() {
                        let depth = tip.saturating_sub(u.height);
                        let tex = if depth >= 6 {
                            confirmed.as_ref()
                        } else {
                            pending.as_ref()
                        };
                        if let Some(tx) = tex {
                            ui.add(img(tx, Vec2::splat(16.0)))
                                .on_hover_text(format!("{} confirmations", depth));
                        } else {
                            ui.label("");
                        }
                        ui.label(fmt_unix(u.tx_time));
                        ui.label(if u.prev_kind == "coinbase" {
                            "Mined"
                        } else if u.prev_kind == "coinstake" {
                            "Stake reward"
                        } else {
                            "Received with"
                        });
                        if st.options.display_addresses {
                            ui.label(&u.address);
                        } else {
                            ui.label(u.txid_display().chars().take(20).collect::<String>());
                        }
                        ui.label(format!("+{} {}", s::format_tri(u.value_sats), s::TICKER))
                            .on_hover_text(format!("txid {} : {}", u.txid_display(), u.n));
                        ui.end_row();
                    }
                });
        });
    }

    fn page_staking(&mut self, ui: &mut egui::Ui) {
        let st_icon = self.tex(ui.ctx(), "icons/staking_off");
        let st = &mut self.st;
        ui.heading("Staking");
        ui.label(
            "read-only status — the tri7 engine mints dry-run blocks only; nothing is broadcast",
        );
        ui.separator();
        let Some(stake) = st.stake.clone() else {
            ui.label("load a wallet to see staking status");
            return;
        };
        if let Some(tex) = &st_icon {
            ui.add(img(tex, Vec2::splat(32.0)));
        }
        ui.label("Staking is currently off on this chain (static tip, staking=0).");
        egui::Grid::new("stake_grid").num_columns(2).show(ui, |ui| {
            ui.label("Current weight (mature):");
            ui.label(format!(
                "{} {} ({} coins ≥ min-age {}s)",
                s::format_tri(stake.mature_weight_sats),
                s::TICKER,
                stake.stakeable_coins,
                STAKE_MIN_AGE
            ));
            ui.end_row();
            ui.label("PoS difficulty (tip nBits):");
            ui.label(stake.pos_difficulty.to_string());
            ui.end_row();
            ui.label("Stake modifier (tip):");
            ui.label(format!(
                "{:016x}{}",
                stake.modifier,
                if stake.modifier_generated {
                    " (generated)"
                } else {
                    ""
                }
            ));
            ui.end_row();
            ui.label("Note:");
            ui.label(&stake.note);
            ui.end_row();
        });
        ui.separator();
        ui.label(egui::RichText::new("Last kernel-search dry run (stake_dryrun.json)").strong());
        match &stake.dryrun {
            Some(d) => {
                egui::Grid::new("dryrun_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Attempts:");
                        ui.label(d.attempts.to_string());
                        ui.end_row();
                        ui.label("Kernel hits:");
                        ui.label(d.kernel_hits.to_string());
                        ui.end_row();
                        ui.label("Coins considered / meeting min-age:");
                        ui.label(format!(
                            "{} / {}",
                            d.coins_considered, d.coins_meeting_min_age
                        ));
                        ui.end_row();
                        ui.label("Reward (last solve):");
                        ui.label(format!(
                            "{} {} (split: {})",
                            s::format_tri(d.reward_sats),
                            s::TICKER,
                            d.split
                        ));
                        ui.end_row();
                        ui.label("Candidate block:");
                        ui.label(&d.block_hash);
                        ui.end_row();
                    });
            }
            None => {
                ui.label("no dry-run result found (run src/bin/stake_mint.rs to produce one)");
            }
        }
    }

    fn page_messages(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Messages");
        ui.label("View and Send Encrypted messages");
        ui.separator();
        ui.label(
            "Encrypted messages (smsg) are carried by the legacy daemon's message store — this page \
             becomes functional with daemon integration. Message ownership can already be proven \
             today: use Sign Message / Verify Message.",
        );
        ui.horizontal(|ui| {
            if ui.button("Sign &Message").clicked() {
                st.goto(Screen::SignMessage);
            }
            if ui.button("&Verify Message").clicked() {
                st.goto(Screen::VerifyMessage);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Block explorer (local tri7d RPC only — never phones home)
    // -----------------------------------------------------------------------

    fn page_explorer(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Block Explorer");
        ui.label(egui::RichText::new(
            "full-chain search from the local tri7d RPC — no external explorer, no outbound traffic",
        ).small());
        ui.separator();
        // --- search box (prominent) ---
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Search:").size(16.0));
            let inp = &mut st.explorer.input;
            let resp = ui.add_sized(
                [520.0, 26.0],
                egui::TextEdit::singleline(inp)
                    .hint_text("address / txid / block height / block hash"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                st.explorer_search_action();
            }
            if ui.button("Search").clicked() {
                st.explorer_search_action();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Stats").clicked() {
                st.explorer_stats_action(20);
            }
            if ui.button("Rich List").clicked() {
                st.explorer_rich_action(25);
            }
            ui.label(
                egui::RichText::new(format!(
                    "endpoint: {}",
                    st.explorer
                        .endpoint
                        .and_then(|i| st.explorer.endpoints.get(i))
                        .map(|s| s.clone())
                        .unwrap_or_else(|| "not probed".into())
                ))
                .small(),
            );
        });
        if let Some(e) = &st.explorer.last_error {
            ui.colored_label(RED_ERR, e);
        }
        ui.separator();

        match st.explorer.view.clone() {
            ex::ExplorerView::Landing => {
                ui.label("Search for an address, transaction, or block — or open the Stats panel.");
            }
            ex::ExplorerView::Address => self.explorer_address_view(ui),
            ex::ExplorerView::Tx => self.explorer_tx_view(ui),
            ex::ExplorerView::Block => self.explorer_block_view(ui),
            ex::ExplorerView::Stats => self.explorer_stats_view(ui),
            ex::ExplorerView::Rich => self.explorer_rich_view(ui),
        }
    }

    fn explorer_address_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some((addr_str, av)) = st
            .explorer
            .addr
            .iter()
            .next()
            .map(|(k, v)| (k.clone(), v.clone()))
        else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&addr_str).strong());
            if ui.button("copy").clicked() {
                ui.output_mut(|o| o.copied_text = addr_str.clone());
            }
        });
        egui::Grid::new("ex_addr_grid").num_columns(2).show(ui, |ui| {
            ui.label("Valid:");
            ui.label(av.valid.to_string());
            ui.end_row();
            ui.label("Kind:");
            ui.label(av.kind.clone().unwrap_or_else(|| "n/a".into()));
            ui.end_row();
            ui.label("In this wallet:");
            ui.label(av
                .ismine
                .map(|b| b.to_string())
                .unwrap_or_else(|| "n/a".into()));
            ui.end_row();
            ui.label("Received (regular txs, all-time):");
            ui.label(av
                .received_tri
                .map(|r| format!("{} {}", s::format_tri((r * 1_000_000.0).round() as i64), s::TICKER))
                .unwrap_or_else(|| "n/a".into()))
                .on_hover_text("getreceivedbyaddress <addr> 0 — regular txs, P2PKH scripts only (legacy daemon semantics)");
            ui.end_row();
            ui.label("Received (snapshot):");
            ui.label(av
                .received_snapshot_sats
                .map(|r| format!("{} {} (h{} coverage)", s::format_tri(r), s::TICKER, st.explorer.rich.as_ref().map(|r| r.height).unwrap_or(2_201_090)))
                .unwrap_or_else(|| "n/a".into()));
            ui.end_row();
            ui.label("Balance (snapshot):");
            ui.label(av
                .balance_snapshot_sats
                .map(|r| format!("{} {}", s::format_tri(r), s::TICKER))
                .unwrap_or_else(|| "n/a".into()));
            ui.end_row();
        });
        ui.separator();
        ui.label(egui::RichText::new("Recent activity (backward block scan)").strong());
        if st.explorer.scanning {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("scanning blocks…");
            });
        }
        egui::Grid::new("ex_addr_hist")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for hit in av.history.iter().rev().take(200).rev() {
                    ui.label(egui::RichText::new(format!("h{}", hit.height)).small());
                    ui.label(
                        egui::RichText::new(if hit.received { "Received" } else { "Sent" }).small(),
                    );
                    let txid_short: String = hit.txid.chars().take(24).collect();
                    if ui.selectable_label(false, txid_short).clicked() {
                        if st.explorer.fetch_tx(&hit.txid).is_ok() {
                            st.explorer.open_tx = Some(hit.txid.clone());
                            st.explorer.view = ex::ExplorerView::Tx;
                        } else {
                            st.explorer.last_error = Some("tx fetch failed".into());
                        }
                    }
                    ui.label(format!(
                        "{} {}",
                        if hit.received { "+" } else { "−" },
                        s::format_tri(hit.sats)
                    ));
                    ui.end_row();
                }
            });
        ui.label(
            egui::RichText::new(format!(
                "scanned blocks {}..{} ({} blocks, {} hits){}",
                av.oldest_scanned
                    .map(|o| o.to_string())
                    .unwrap_or_else(|| "?".into()),
                av.newest_scanned
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into()),
                av.blocks_scanned,
                av.history.len(),
                if av.exhausted {
                    " — reached height 1"
                } else {
                    ""
                }
            ))
            .small(),
        );
        if !av.exhausted {
            let busy = st.explorer.scanning;
            if ui
                .add_enabled(!busy, egui::Button::new("Scan 300 more blocks"))
                .clicked()
            {
                st.explorer_attach_snapshot(&addr_str);
                st.explorer_scan_more_action(st.explorer.scan_budget_blocks);
            }
        }
    }

    fn explorer_tx_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some(txid) = st.explorer.open_tx.clone() else {
            return;
        };
        let Some(tx) = st.explorer.txs.get(&txid).cloned() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Transaction").strong());
            ui.label(egui::RichText::new(&txid).monospace().small());
            if ui.button("copy txid").clicked() {
                ui.output_mut(|o| o.copied_text = txid.clone());
            }
        });
        egui::Grid::new("ex_tx_grid").num_columns(2).show(ui, |ui| {
            ui.label("Confirmations:");
            ui.label(
                tx.confirmations
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "n/a".into()),
            );
            ui.end_row();
            ui.label("Block height:");
            ui.label(
                tx.height
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| "unconfirmed".into()),
            );
            ui.end_row();
            ui.label("Timestamp:");
            ui.label(fmt_unix(tx.time));
            ui.end_row();
            ui.label("Size:");
            ui.label(format!("{} bytes", tx.size));
            ui.end_row();
        });
        if let Some(bh) = &tx.blockhash {
            ui.horizontal(|ui| {
                ui.label("Block:");
                let short: String = bh.chars().take(30).collect();
                if ui.selectable_label(false, short).clicked() {
                    if let Ok(b) = st.explorer.fetch_block_by_hash(bh) {
                        st.explorer.open_block = Some(b.height);
                        st.explorer.view = ex::ExplorerView::Block;
                    }
                }
            });
        }
        // fee / stake credit
        let fee = st.fee_of(&txid);
        match fee {
            Some(f) if ex::tx_is_coinstake(&tx) => ui.colored_label(
                OK_GRN,
                format!("STAKE — credit {} {}", s::format_tri(-f), s::TICKER),
            ),
            Some(f) => ui.label(format!("Fee: {} {}", s::format_tri(f), s::TICKER)),
            None => ui.label("Fee: n/a (coinbase / prevouts unresolved)"),
        };
        ui.separator();
        ui.label(egui::RichText::new("Inputs").strong());
        egui::Grid::new("ex_vin")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for vin in &tx.vin {
                    match (&vin.prev_txid, vin.prev_n) {
                        (Some(pt), n) => {
                            let short: String = pt.chars().take(24).collect();
                            if ui
                                .selectable_label(false, format!("{}:{}", short, n.unwrap_or(0)))
                                .clicked()
                            {
                                if st.explorer.fetch_tx(pt).is_ok() {
                                    st.explorer.open_tx = Some(pt.clone());
                                    st.explorer.view = ex::ExplorerView::Tx;
                                }
                            }
                            ui.label("spent prevout");
                        }
                        (None, _) => {
                            ui.label("coinbase");
                            ui.label(vin.coinbase.clone().unwrap_or_default());
                        }
                    }
                    ui.label(egui::RichText::new("").small());
                    ui.end_row();
                }
            });
        ui.label(egui::RichText::new("Outputs").strong());
        egui::Grid::new("ex_vout")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for vo in &tx.vout {
                    ui.label(format!("{}", vo.n));
                    ui.label(format!("{} {}", s::format_tri(vo.value_sats), s::TICKER));
                    ui.label(&vo.script_type);
                    let addr = ex::addr_of_vout(vo).unwrap_or_else(|| "—".into());
                    let addr2 = addr.clone();
                    if ui.selectable_label(false, addr).clicked() && addr2 != "—" {
                        if st.explorer.open_address_action(&addr2).is_ok() {
                            st.explorer_attach_snapshot(&addr2);
                            if let Ok(_n) = st
                                .explorer
                                .scan_address_more(&addr2, st.explorer.scan_budget_blocks)
                            {
                                st.explorer.view = ex::ExplorerView::Address;
                            }
                        }
                    }
                    ui.end_row();
                }
            });
    }

    fn explorer_block_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some(block_h) = st.explorer.open_block else {
            return;
        };
        let Some(b) = st.explorer.blocks.get(&block_h).cloned() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Block {}", b.height)).strong());
            if let Some(prev) = &b.prev {
                if ui.button("← prev").clicked() {
                    if let Some(&h) = st.explorer.block_height_of_hash.get(prev) {
                        if st.explorer.fetch_block_by_height(h).is_ok() {
                            st.explorer.open_block = Some(h);
                            st.explorer.view = ex::ExplorerView::Block;
                        }
                    }
                }
            }
            if let Some(next) = &b.next {
                if ui.button("next →").clicked() {
                    if let Some(&h) = st.explorer.block_height_of_hash.get(next) {
                        if st.explorer.fetch_block_by_height(h).is_ok() {
                            st.explorer.open_block = Some(h);
                            st.explorer.view = ex::ExplorerView::Block;
                        }
                    }
                }
            }
        });
        egui::Grid::new("ex_blk_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Hash:");
                ui.label(egui::RichText::new(&b.hash).monospace().small());
                ui.end_row();
                ui.label("Timestamp:");
                ui.label(fmt_unix(b.time));
                ui.end_row();
                ui.label("nBits:");
                ui.label(&b.bits);
                ui.end_row();
                ui.label("Difficulty:");
                ui.label(b.difficulty.to_string());
                ui.end_row();
                ui.label("Type:");
                ui.label(if b.pos {
                    "proof-of-stake"
                } else {
                    "proof-of-work"
                });
                ui.end_row();
                ui.label("Stake modifier:");
                ui.label(format!(
                    "{}{}",
                    b.modifier.clone().unwrap_or_else(|| "n/a".into()),
                    if b.stake_modifier_generated {
                        " (generated)"
                    } else {
                        ""
                    }
                ));
                ui.end_row();
                ui.label("Modifier checksum:");
                ui.label(b.modifierchecksum.clone().unwrap_or_else(|| "n/a".into()));
                ui.end_row();
                ui.label("Entropy bit:");
                ui.label(
                    b.entropybit
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "n/a".into()),
                );
                ui.end_row();
                ui.label("Size / Version / Nonce:");
                ui.label(format!("{} B / v{} / {}", b.size, b.version, b.nonce));
                ui.end_row();
                ui.label("Confirmations:");
                ui.label(
                    b.confirmations
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "n/a".into()),
                );
                ui.end_row();
            });
        ui.separator();
        ui.label(egui::RichText::new(format!("Transactions ({})", b.txids.len())).strong());
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for txid in &b.txids {
                    let short: String = txid.chars().take(30).collect();
                    let clicked = ui.selectable_label(false, short).clicked();
                    if clicked {
                        if st.explorer.fetch_tx(txid).is_ok() {
                            st.explorer.open_tx = Some(txid.clone());
                            st.explorer.view = ex::ExplorerView::Tx;
                        } else {
                            st.explorer.last_error = Some("tx fetch failed".into());
                        }
                    }
                }
            });
    }

    fn explorer_stats_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some(stats) = st.explorer.stats.clone() else {
            return;
        };
        ui.label(egui::RichText::new("Triangles Core").strong());
        egui::Grid::new("ex_stats").num_columns(2).show(ui, |ui| {
            let g = |k: &str| stats.get(k).cloned().unwrap_or(Value::Null);
            ui.label("Client:");
            ui.label(g("subversion").as_str().unwrap_or("n/a").to_string());
            ui.end_row();
            ui.label("Money supply:");
            ui.label(format!(
                "{} {} (coverage h{})",
                stats
                    .get("moneysupply")
                    .and_then(|x| x.as_f64())
                    .map(|f| format!("{}", f))
                    .unwrap_or_else(|| "n/a".into()),
                s::TICKER,
                stats
                    .get("moneysupply_coverage_height")
                    .and_then(|x| x.as_u64())
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "?".into())
            ));
            ui.end_row();
            ui.label("Tip height:");
            ui.label(g("blocks").to_string());
            ui.end_row();
            ui.label("Difficulty (PoS / PoW):");
            ui.label(format!(
                "{} / {}",
                stats
                    .pointer("/difficulty/proof-of-stake")
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "?".into()),
                stats
                    .pointer("/difficulty/proof-of-work")
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "?".into())
            ));
            ui.end_row();
            ui.label("Connections:");
            ui.label(g("connections").to_string());
            ui.end_row();
            ui.label("Testnet:");
            ui.label(g("testnet").to_string());
            ui.end_row();
        });
        ui.separator();
        ui.label(egui::RichText::new("Latest blocks").strong());
        egui::Grid::new("ex_latest")
            .num_columns(5)
            .striped(true)
            .show(ui, |ui| {
                for b in st.explorer.latest.clone().iter().rev() {
                    ui.label(format!("h{}", b.height));
                    ui.label(if b.pos { "PoS" } else { "PoW" });
                    let hash_short: String = b.hash.chars().take(24).collect();
                    let b2 = b.clone();
                    if ui.selectable_label(false, hash_short).clicked() {
                        if st.explorer.fetch_block_by_height(b2.height).is_ok() {
                            st.explorer.open_block = Some(b2.height);
                            st.explorer.view = ex::ExplorerView::Block;
                        }
                    }
                    ui.label(fmt_unix(b.time));
                    ui.label(if b.pos { "stake reward block" } else { "mined" });
                    ui.end_row();
                }
            });
    }

    fn explorer_rich_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some(rl) = st.explorer.rich.clone() else {
            return;
        };
        ui.label(egui::RichText::new(format!("Rich list — top {}", rl.entries.len())).strong());
        ui.label(egui::RichText::new(&rl.source).small());
        ui.separator();
        egui::Grid::new("ex_rich")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.label("#");
                ui.label("Address");
                ui.label("Balance");
                ui.label("% of supply");
                ui.end_row();
                for (i, (addr, sats)) in rl.entries.iter().enumerate() {
                    ui.label(format!("{}", i + 1));
                    let a2 = addr.clone();
                    if ui.selectable_label(false, addr.clone()).clicked()
                        && st.explorer.open_address_action(&a2).is_ok()
                    {
                        st.explorer.view = ex::ExplorerView::Address;
                    }
                    ui.label(format!("{} {}", s::format_tri(*sats), s::TICKER));
                    ui.label(format!(
                        "{:.4}%",
                        if rl.total_chain_sats > 0 {
                            *sats as f64 / rl.total_chain_sats as f64 * 100.0
                        } else {
                            0.0
                        }
                    ));
                    ui.end_row();
                }
            });
    }

    fn page_sign(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Sign Message");
        ui.label("Sign a message to prove you own a TRI address");
        ui.separator();
        let addrs: Vec<String> = {
            let mut v: Vec<String> = st.key_map().keys().cloned().collect();
            v.sort();
            v
        };
        egui::ComboBox::from_id_source("sign_addr")
            .selected_text(if st.sign.address.is_empty() {
                "select address".to_string()
            } else {
                st.sign.address.clone()
            })
            .width(420.0)
            .show_ui(ui, |ui| {
                for a in &addrs {
                    let addr = &mut st.sign.address;
                    ui.selectable_value(addr, a.clone(), a.clone());
                }
            });
        ui.label("Message:");
        let msg = &mut st.sign.message;
        egui::TextEdit::multiline(msg).desired_rows(4).show(ui);
        if ui.button("Sign &Message").clicked() {
            st.sign_message_action();
        }
        if let Some(e) = &st.sign.error {
            ui.colored_label(RED_ERR, e);
        }
        if !st.sign.signature.is_empty() {
            ui.label("Signature:");
            ui.add(
                egui::Label::new(egui::RichText::new(&st.sign.signature).monospace().small())
                    .wrap(),
            );
            if ui.button("Copy signature").clicked() {
                ui.output_mut(|o| o.copied_text = st.sign.signature.clone());
            }
        }
    }

    fn page_verify(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Verify Message");
        ui.label("Verify a message to ensure it was signed with a specified TRI address");
        ui.separator();
        egui::Grid::new("verify_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Address:");
                let a = &mut st.verify.address;
                ui.add_sized([420.0, 22.0], egui::TextEdit::singleline(a));
                ui.end_row();
                ui.label("Signature:");
                let sg = &mut st.verify.signature;
                ui.add_sized([420.0, 22.0], egui::TextEdit::singleline(sg));
                ui.end_row();
            });
        ui.label("Message:");
        let msg = &mut st.verify.message;
        egui::TextEdit::multiline(msg).desired_rows(4).show(ui);
        if ui.button("&Verify Message").clicked() {
            st.verify_message_action();
        }
        match &st.verify.result {
            Some(Ok(true)) => ui.colored_label(
                OK_GRN,
                "Message verified — the signature matches the address",
            ),
            Some(Ok(false)) => ui.colored_label(RED_ERR, "Message verification FAILED"),
            Some(Err(e)) => ui.colored_label(RED_ERR, e),
            None => ui.label(""),
        };
    }

    fn page_options(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        ui.heading("Options");
        ui.separator();
        ui.label(egui::RichText::new("Main").strong());
        egui::Grid::new("opt_main").num_columns(2).show(ui, |ui| {
            ui.label("Pay transaction &fee:");
            ui.horizontal(|ui| {
                let f = &mut st.options.pay_fee;
                ui.add_sized([140.0, 22.0], egui::TextEdit::singleline(f));
                ui.label(s::TICKER);
            });
            ui.end_row();
            ui.label("Reserve:");
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut st.options.reserve).desired_width(140.0),
            )
            .on_hover_text("requires daemon integration (staking reserve)");
            ui.end_row();
        });
        ui.label(egui::RichText::new("Network").strong());
        let mut off = false;
        ui.add_enabled(false, egui::Checkbox::new(&mut off, "Map port using &UPnP"))
            .on_hover_text("requires daemon integration");
        ui.label(egui::RichText::new("Window").strong());
        let mut off2 = false;
        ui.add_enabled(
            false,
            egui::Checkbox::new(&mut off2, "&Minimize to the tray instead of the taskbar"),
        )
        .on_hover_text("not supported by the egui shell yet");
        ui.label(egui::RichText::new("Display").strong());
        ui.checkbox(
            &mut st.options.display_addresses,
            "&Display addresses in transaction list",
        );
        ui.checkbox(
            &mut st.options.coin_control,
            "Display coin &control features (experts only!)",
        );
        ui.separator();
        if ui.button("&Apply").clicked() {
            match st.save_options_action() {
                Ok(()) => st.status_log.push("options saved".into()),
                Err(e) => st.status_log.push(format!("options: {}", e)),
            }
        }
        if ui.button("&OK").clicked() {
            let _ = st.save_options_action();
            st.goto(Screen::Overview);
        }
        for line in st.status_log.iter().rev().take(2) {
            ui.label(egui::RichText::new(line).small());
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.st.dialogs.about {
            let mut open = true;
            egui::Window::new("About Cryptographic Triangles")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    if let Some(tex) = self.tex(ui.ctx(), "images/about") {
                        ui.add(img(&tex, Vec2::new(200.0, 200.0)));
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "Triangles version tri7-core v{} (phase 6 GUI)",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .strong(),
                    );
                    ui.label("Modern client engine — byte-exact replay of the legacy chain");
                    if ui.button("OK").clicked() {
                        self.st.dialogs.about = false;
                    }
                });
            self.st.dialogs.about = open;
        }
        if let Some(msg) = self.st.dialogs.encrypt.clone() {
            let mut open = true;
            egui::Window::new("Encrypt Wallet")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(msg);
                    if ui.button("OK").clicked() {
                        self.st.dialogs.encrypt = None;
                    }
                });
            if !open {
                self.st.dialogs.encrypt = None;
            }
        }
        if self.st.dialogs.qr {
            let mut open = true;
            egui::Window::new("Request Payment")
                .open(&mut open)
                .show(ctx, |ui| {
                    let st = &mut self.st;
                    if let Some(qr) = st.receive.qr.clone() {
                        let (w, h, px) = s::qr_pixels(&qr, 4);
                        let img_data = egui::ColorImage::from_rgba_unmultiplied(
                            [w, h],
                            px.iter()
                                .flat_map(|g| [*g, *g, *g, 0xff])
                                .collect::<Vec<_>>()
                                .as_slice(),
                        );
                        let tex = ctx.load_texture("qr_render", img_data, TextureOptions::NEAREST);
                        ui.add(img(&tex, Vec2::splat(260.0)));
                        ui.label(egui::RichText::new(&st.receive.uri).small());
                        ui.horizontal(|ui| {
                            if ui.button("Copy URI").clicked() {
                                ui.output_mut(|o| o.copied_text = st.receive.uri.clone());
                            }
                            let path = &mut st.dialogs.qr_save_path;
                            ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(path));
                            if ui.button("&Save As...").clicked()
                                && !st.dialogs.qr_save_path.is_empty()
                            {
                                let p = st.dialogs.qr_save_path.clone();
                                match st.save_qr_action(&p) {
                                    Ok(n) => st
                                        .status_log
                                        .push(format!("QR PNG saved to {} ({} bytes)", p, n)),
                                    Err(e) => st.status_log.push(format!("QR save failed: {}", e)),
                                }
                            }
                        });
                    }
                });
            self.st.dialogs.qr = open;
        }
    }

    fn debug_window(&mut self, ctx: &egui::Context) {
        if !self.console_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Triangles - Debug window")
            .open(&mut open)
            .default_width(680.0)
            .show(ctx, |ui| {
                let st = &mut self.st;
                ui.horizontal(|ui| {
                    if ui.button("&Information (refresh)").clicked() {
                        st.refresh_debug_info_action();
                    }
                    ui.label(egui::RichText::new("&Console").strong());
                });
                ui.separator();
                let di = st.debug_info.clone().unwrap_or_default();
                egui::Grid::new("dbg_info").num_columns(2).show(ui, |ui| {
                    ui.label("Client name:");
                    ui.label("tri7-core");
                    ui.end_row();
                    ui.label("Client version:");
                    ui.label(format!("v{} (phase 6 GUI)", env!("CARGO_PKG_VERSION")));
                    ui.end_row();
                    ui.label("Crypto:");
                    ui.label("rust secp256k1 (RFC6979, recovery)");
                    ui.end_row();
                    ui.label("Number of connections:");
                    ui.label(di
                        .connections
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| di.error.clone().unwrap_or_else(|| "n/a".into())));
                    ui.end_row();
                    ui.label("Current number of blocks:");
                    ui.label(st.tip.as_ref().map(|x| x.height.to_string()).unwrap_or_else(|| "n/a".into()));
                    ui.end_row();
                    ui.label("Last block time:");
                    ui.label(st.tip.as_ref().map(|x| fmt_unix(x.tip_time)).unwrap_or_else(|| "n/a".into()));
                    ui.end_row();
                    ui.label("Debug log file:");
                    ui.label("tri7d journald unit (not a file on this host)");
                    ui.end_row();
                });
                ui.separator();
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for e in st.console.iter().rev() {
                        ui.label(egui::RichText::new(format!("> {}", e.request)).strong().small());
                        if let Some(r) = &e.reply {
                            ui.label(egui::RichText::new(r).small());
                        }
                        if let Some(err) = &e.error {
                            ui.colored_label(RED_ERR, err);
                        }
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(">");
                    let inp = &mut st.console_input;
                    let resp = ui.add_sized([520.0, 22.0], egui::TextEdit::singleline(inp));
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        st.rpc_console_send_action();
                    }
                    if ui.button("Send").clicked() {
                        st.rpc_console_send_action();
                    }
                    if ui.button("Clear console").clicked() {
                        st.console.clear();
                    }
                });
                ui.label(egui::RichText::new(
                    "read-only surface: gettip getblockcount getblockhash getblock getrawtransaction getinfo getdifficulty getconnectioncount getstakemodifier getbalance listunspent listwalletaddresses getreceivedbyaddress validateaddress health",
                )
                .small());
            });
        self.console_open = open;
    }
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn run(cfg: UiConfig) {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 740.0])
            .with_title("Cryptographic Triangles")
            .with_icon(load_icon()),
        ..Default::default()
    };
    let app = Box::new(|cc: &eframe::CreationContext| {
        Tri7App::apply_theme(&cc.egui_ctx);
        Ok::<Box<dyn eframe::App>, Box<dyn std::error::Error + Send + Sync>>(Box::new(Tri7App {
            st: AppState::new(cfg),
            tex: HashMap::new(),
            console_open: false,
        }))
    });
    if let Err(e) = eframe::run_native("Cryptographic Triangles", opts, app) {
        eprintln!("eframe failed: {}", e);
        std::process::exit(1);
    }
}
