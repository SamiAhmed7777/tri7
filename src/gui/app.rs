//! egui view layer — the historical Cryptographic Triangles Qt wallet,
//! rebuilt cleanly for egui (REDESIGN_SPEC.md).
//!
//! Identity anchors preserved from triangles_v5/src/qt (v6.2.6.7):
//!   - black application shell, #e32105 brand accent, red/brown/gold palette
//!     DNA (production palette: gui::theme — spec §3);
//!   - strong horizontal header (64 px + brand lockup), left navigation
//!     (216 px, grouped), central work area, bottom network/status strip;
//!   - original primary page order and original wallet terminology;
//!   - legacy menu_16/status icons carried over as functional fallbacks
//!     (assets/icons, assets/menu_16) plus the reconstructed brand lockups
//!     (assets/brand — spec §11).
//!
//! View-layer only: ALL behavior comes from gui::app_state::AppState (pure,
//! tested headless) — this file renders it and forwards clicks to its action
//! methods. No network broadcast exists anywhere; the only outbound traffic
//! is read-only JSON-RPC to the local tri7d (via AppState).

use crate::gui::app_state::AppState;
use crate::gui::explorer as ex;
use crate::gui::state as s;
use crate::gui::state::{Screen, UiConfig, UtxoRec};
use crate::gui::theme as th;
use crate::updater::UpdatePhase as UpdatePhaseKind;
use eframe::egui;
use egui::{
    Color32, Rounding, Sense, Stroke, TextureHandle, TextureOptions, Vec2,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Size a texture into an egui Image (egui 0.29 SizedTexture path).
fn img(tex: &TextureHandle, size: Vec2) -> egui::Image<'static> {
    egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), size))
}

// ---------------------------------------------------------------------------
// Assets — legacy PNGs (functional fallbacks) + reconstructed brand lockups
// (spec §11: brand JPEGs stay in brand/ as references, never rendered raw)
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
    "menu_16/staking" => "menu_16/staking.png",
    "icons/triangles" => "icons/triangles.png",
    "icons/triangles-16" => "icons/triangles-16.png",
    "icons/triangles-128" => "icons/triangles-128.png",
    "icons/staking_off" => "icons/staking_off.png",
    "icons/staking_on" => "icons/staking_on.png",
    "icons/lock_open" => "icons/lock_open.png",
    "icons/lock_closed" => "icons/lock_closed.png",
    "icons/synced" => "icons/synced.png",
    "icons/notsynced" => "icons/notsynced.png",
    "icons/connect0" => "icons/connect0_16.png",
    "icons/connect1" => "icons/connect1_16.png",
    "icons/connect2" => "icons/connect2_16.png",
    "icons/connect3" => "icons/connect3_16.png",
    "icons/connect4" => "icons/connect4_16.png",
    "icons/tx_confirmed" => "icons/transaction2.png",
    "icons/tx_mined" => "icons/tx_mined.png",
    "icons/tx_inout" => "icons/tx_inout.png",
    "icons/tx_conflict" => "icons/tx_conflict.png",
    "icons/clock1" => "icons/clock1.png",
    "icons/clock2" => "icons/clock2.png",
    "icons/clock3" => "icons/clock3.png",
    "icons/clock4" => "icons/clock4.png",
    "icons/clock5" => "icons/clock5.png",
    "icons/refresh" => "icons/refresh.png",
    "brand/header" => "brand/brand_header_new@2x.png",
    "brand/mark" => "brand/brand_mark_32.png",
    "brand/mark16" => "brand/brand_mark_16.png",
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

fn truncate_chars(x: &str, n: usize) -> String {
    x.chars().take(n).collect()
}

static DEBUG_TOGGLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Send page step rail (spec §10.2) — view state, driven by AppState state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SendStep {
    Details,
    Review,
    Broadcast,
}

/// Messages segmented control (spec §10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsgTab {
    Inbox,
    Sent,
    Compose,
}

/// Block Explorer tabs (spec §10.8) — the underlying ExplorerView data paths
/// are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExTab {
    Overview,
    Block,
    Transaction,
    Address,
    Rich,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct Tri7App {
    st: AppState,
    tex: HashMap<String, TextureHandle>,
    console_open: bool,
    /// repaint ticks since start — paces the live-panel polls (10 ticks ≈ 5s)
    poll_ticker: u32,

    // ---- view-only state (spec §10/§12.3; no engine logic) ----
    send_step: SendStep,
    msg_tab: MsgTab,
    msg_selected: Option<usize>,
    tx_selected: Option<usize>,
    tx_search: String,
    tx_min_amount: String,
    tx_type_filter: usize,
    tx_status_filter: usize,
    ab_selected: Option<String>,
    ab_search: String,
    ab_source_filter: usize,
    ab_edit: Option<(String, String)>,
    ab_delete: Option<String>,
    ab_new_label: String,
    options_tab: usize,
    options_return: Option<Screen>,
    ex_tab: ExTab,
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

    /// QR texture cached by URI (spec §12.5: never recreate every frame).
    fn qr_texture(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        let qr = self.st.receive.qr.clone()?;
        let uri = self.st.receive.uri.clone();
        let key = format!("qr/{}", uri);
        if let Some(x) = self.tex.get(&key) {
            return Some(x.clone());
        }
        let (w, h, px) = s::qr_pixels(&qr, 4);
        let img_data = egui::ColorImage::from_rgba_unmultiplied(
            [w, h],
            px.iter().flat_map(|g| [*g, *g, *g, 0xff]).collect::<Vec<_>>().as_slice(),
        );
        let handle = ctx.load_texture(key.clone(), img_data, TextureOptions::NEAREST);
        self.tex.insert(key, handle.clone());
        Some(handle)
    }

    fn ico(&mut self, ui: &mut egui::Ui, key: &str, size: f32) {
        if let Some(tex) = self.tex(ui.ctx(), key) {
            ui.add(img(&tex, Vec2::splat(size)));
        }
    }

    fn apply_theme(ctx: &egui::Context) {
        th::apply(ctx);
    }

    // -----------------------------------------------------------------------
    // Header (spec §5/§8): 64 px, brand lockup left, File/Settings/Help right,
    // 1 px BRAND_RED bottom rule at 35% opacity. Operations menu removed.
    // -----------------------------------------------------------------------
    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("wHeader")
            .exact_height(64.0)
            .frame(egui::Frame::none().fill(th::WINDOW_BG))
            .show(ctx, |ui| {
                // banner LEFT-aligned in the header strip — the legacy
                // mainwindow.ui header placement (brand left, menus right)
                if let Some(tex) = self.tex(ui.ctx(), "brand/header") {
                    ui.add_space(20.0);
                    ui.add(img(&tex, Vec2::new(219.0, 46.0))); // 1199x252 aspect-true
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    self.menu_bar(ui);
                });
            });
        // subtle 1 px BRAND_RED bottom rule at 35% opacity
        egui::TopBottomPanel::top("wHeaderRule")
            .exact_height(1.0)
            .frame(egui::Frame::none().fill(Color32::from_rgba_unmultiplied(
                0xE3, 0x21, 0x05, 89,
            )))
            .show(ctx, |_ui| {});
    }

    /// Historical menu colors (spec §8): MENU_RED surface, MENU_TEXT items,
    /// MENU_SELECTED selection, 6 px radius, 6 px padding.
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.scope(|ui| {
            let v = &mut ui.style_mut().visuals;
            v.window_fill = th::MENU_RED;
            v.window_stroke = Stroke::new(1.0, th::MENU_SELECTED);
            self.menu_file(ui);
            self.menu_settings(ui);
            self.menu_help(ui);
        });
    }

    fn scoped_menu_button<R>(
        ui: &mut egui::Ui,
        label: &str,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<R> {
        let mut inner = None;
        ui.menu_button(label, |ui| {
            let v = &mut ui.style_mut().visuals.widgets;
            v.noninteractive.fg_stroke = Stroke::new(1.0, th::MENU_TEXT);
            v.inactive.fg_stroke = Stroke::new(1.0, th::MENU_TEXT);
            v.inactive.weak_bg_fill = Color32::TRANSPARENT;
            v.inactive.bg_fill = Color32::TRANSPARENT;
            v.inactive.bg_stroke = Stroke::NONE;
            v.hovered.weak_bg_fill = th::MENU_SELECTED;
            v.hovered.bg_fill = th::MENU_SELECTED;
            v.hovered.bg_stroke = Stroke::NONE;
            v.hovered.fg_stroke = Stroke::new(1.0, th::MENU_SELECTED_TEXT);
            v.active.weak_bg_fill = th::MENU_SELECTED;
            v.active.bg_fill = th::MENU_SELECTED;
            v.active.bg_stroke = Stroke::NONE;
            v.active.fg_stroke = Stroke::new(1.0, th::MENU_SELECTED_TEXT);
            inner = Some(add(ui));
        });
        inner
    }

    fn menu_file(&mut self, ui: &mut egui::Ui) {
        Self::scoped_menu_button(ui, "File", |ui| {
            if ui.button("Encrypt Wallet...").clicked() {
                self.st.encrypt_wallet_stub_action();
                ui.close_menu();
            }
            if ui.button("Backup Wallet...").clicked() {
                self.st.goto(Screen::Overview);
                ui.close_menu();
            }
            if ui.button("Sign Message...").clicked() {
                self.st.goto(Screen::SignMessage);
                ui.close_menu();
            }
            if ui.button("Verify Message...").clicked() {
                self.st.goto(Screen::VerifyMessage);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Exit Triangles wallet").clicked() {
                std::process::exit(0);
            }
        });
    }

    fn menu_settings(&mut self, ui: &mut egui::Ui) {
        Self::scoped_menu_button(ui, "Settings", |ui| {
            if ui.button("Options").clicked() {
                if self.st.screen != Screen::Options {
                    self.options_return = Some(self.st.screen);
                }
                self.st.goto(Screen::Options);
                ui.close_menu();
            }
            if ui.button("Debug Window").clicked() {
                DEBUG_TOGGLE.store(true, std::sync::atomic::Ordering::Relaxed);
                ui.close_menu();
            }
        });
    }

    fn menu_help(&mut self, ui: &mut egui::Ui) {
        Self::scoped_menu_button(ui, "Help", |ui| {
            if ui.button("About Cryptographic Triangles").clicked() {
                self.st.dialogs.about = true;
                ui.close_menu();
            }
        });
    }

    // -----------------------------------------------------------------------
    // Sidebar (spec §6): 216 px, grouped — primary six first (exact
    // historical order), 1 px divider + TOOLS group, Options at the bottom.
    // -----------------------------------------------------------------------
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(216.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(th::WINDOW_BG))
            .show(ctx, |ui| {
                ui.add_space(12.0);
                let primary = [
                    Screen::Overview,
                    Screen::Send,
                    Screen::Receive,
                    Screen::AddressBook,
                    Screen::Transactions,
                    Screen::Messages,
                ];
                for sc in primary {
                    self.nav_row(ui, sc);
                }
                ui.add_space(8.0);
                th::hairline(ui);
                th::nav_group_label(ui, "TOOLS");
                let tools = [
                    Screen::Staking,
                    Screen::Explorer,
                    Screen::SignMessage,
                    Screen::VerifyMessage,
                ];
                for sc in tools {
                    self.nav_row(ui, sc);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(12.0);
                    self.nav_row(ui, Screen::Options);
                });
                // right border BORDER_SUBTLE
                let rect = ui.max_rect();
                let edge = egui::Rect::from_min_max(
                    egui::pos2(rect.right() - 1.0, rect.top()),
                    egui::pos2(rect.right(), rect.bottom()),
                );
                ui.painter().rect_filled(edge, Rounding::ZERO, th::BORDER_SUBTLE);
            });
    }

    fn nav_row(&mut self, ui: &mut egui::Ui, screen: Screen) {
        let icon = format!("menu_16/{}", screen.icon());
        let selected = self.st.screen == screen;
        let badge = if screen == Screen::Messages {
            let n = self.st.messages.unread_count();
            if n > 0 { Some(n) } else { None }
        } else {
            None
        };
        let tex = self.tex(ui.ctx(), &icon);
        let mut resp = th::nav_row(ui, tex.as_ref(), screen.label(), badge, selected);
        if resp.clicked() {
            if screen == Screen::Options && self.st.screen != Screen::Options {
                self.options_return = Some(self.st.screen);
            }
            resp.request_focus();
            self.st.goto(screen);
        }
    }

    // -----------------------------------------------------------------------
    // Bottom status bar (spec §9): 40 px dedicated strip — connection,
    // sync, staking, encryption, transport summary, refresh. No duplicate
    // sidebar indicators.
    // -----------------------------------------------------------------------
    fn statusbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("wStatusBar")
            .exact_height(40.0)
            .frame(egui::Frame::none().fill(th::WINDOW_BG))
            .show(ctx, |ui| {
                // 1 px BORDER_SUBTLE top border
                let rect = ui.max_rect();
                let edge = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top()),
                    egui::pos2(rect.right(), rect.top() + 1.0),
                );
                ui.painter().rect_filled(edge, Rounding::ZERO, th::BORDER_SUBTLE);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    // 1. connection cluster — actual 0–4 strength by count
                    let conns = self.st.debug_info.as_ref().and_then(|d| d.connections);
                    let conn_icon = match conns.unwrap_or(0) {
                        0 => "icons/connect0",
                        1..=2 => "icons/connect1",
                        3..=4 => "icons/connect2",
                        5..=6 => "icons/connect3",
                        _ => "icons/connect4",
                    };
                    let conn_color = if conns.unwrap_or(0) == 0 {
                        th::ERROR
                    } else {
                        th::TEXT_MUTED
                    };
                    let conn_tex = self.tex(ui.ctx(), conn_icon);
                    th::status_cluster(
                        ui,
                        conn_tex.as_ref(),
                        &format!("{} connections", conns.unwrap_or(0)),
                        conn_color,
                    );
                    ui.separator();
                    // 2. sync cluster — expands; thin progress bar while syncing
                    let (progress, label) = self.sync_progress();
                    let synced = progress >= 1.0;
                    let sync_icon = if self.st.tip.is_some() && synced {
                        "icons/synced"
                    } else {
                        "icons/notsynced"
                    };
                    let sync_color = if synced {
                        if self.st.debug_info.as_ref().map(|d| d.reachable).unwrap_or(false) {
                            th::SUCCESS
                        } else {
                            th::TEXT_MUTED
                        }
                    } else {
                        th::WARNING
                    };
                    let sync_tex = self.tex(ui.ctx(), sync_icon);
                    th::status_cluster(ui, sync_tex.as_ref(), "Sync", sync_color);
                    let reachable = self
                        .st
                        .debug_info
                        .as_ref()
                        .map(|d| d.reachable)
                        .unwrap_or(false);
                    if synced && reachable {
                        ui.label(
                            egui::RichText::new(format!("Synced · {}", label))
                                .size(11.0)
                                .strong()
                                .color(sync_color),
                        );
                    } else if synced {
                        // local tip known but no tri7d RPC — say so honestly
                        ui.label(
                            egui::RichText::new(label).size(11.0).strong().color(th::TEXT_MUTED),
                        );
                    } else {
                        ui.scope(|ui| {
                            ui.style_mut().visuals.selection.bg_fill = th::BRAND_RED;
                            ui.style_mut()
                                .visuals
                                .selection
                                .stroke = Stroke::new(0.0, Color32::TRANSPARENT);
                            ui.add(
                                egui::ProgressBar::new(progress)
                                    .desired_width(160.0)
                                    .desired_height(4.0),
                            );
                        });
                        ui.label(
                            egui::RichText::new(label).size(11.0).strong().color(sync_color),
                        );
                    }
                    ui.separator();
                    // 3. staking cluster — actual live result
                    let stake = self.st.staking_live.clone();
                    let enabled = stake.field("enabled");
                    let staking_on = enabled == "true" && stake.field("staking") == "true";
                    let (stake_icon, stake_label, stake_color) = if staking_on {
                        ("icons/staking_on", "Staking on", th::SUCCESS)
                    } else if enabled == "true" {
                        ("icons/staking_off", "Staking idle", th::WARNING)
                    } else {
                        ("icons/staking_off", "Staking off", th::TEXT_MUTED)
                    };
                    let stake_tex = self.tex(ui.ctx(), stake_icon);
                    th::status_cluster(ui, stake_tex.as_ref(), stake_label, stake_color);
                    ui.separator();
                    // 4. encryption cluster (wallet encryption is not tracked
                    // by this build — honest idle state)
                    let enc_tex = self.tex(ui.ctx(), "icons/lock_open");
                    th::status_cluster(ui, enc_tex.as_ref(), "Unencrypted", th::TEXT_MUTED);
                    ui.separator();
                    // 5. compact transport summary
                    let transport = self
                        .st
                        .peers_live
                        .table
                        .as_ref()
                        .and_then(|v| v.get("by_transport"))
                        .map(|bt| {
                            format!(
                                "C {} · T {} · I {}",
                                bt.get("clearnet").and_then(|x| x.as_u64()).unwrap_or(0),
                                bt.get("tor").and_then(|x| x.as_u64()).unwrap_or(0),
                                bt.get("i2p").and_then(|x| x.as_u64()).unwrap_or(0),
                            )
                        })
                        .unwrap_or_else(|| "C — · T — · I —".into());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(transport).size(11.0).strong().color(th::TEXT_MUTED),
                        ),
                    )
                    .on_hover_text(
                        "Peer transports: C = clearnet, T = Tor, I = I2P",
                    );
                    // 6. refresh — 28 px icon-only ghost button
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        let refresh_tex = self.tex(ui.ctx(), "icons/refresh");
                        if self.refresh_button(ui, refresh_tex.as_ref()) {
                            self.st.refresh_debug_info_action();
                            self.st.staking_poll_action();
                            self.st.peers_poll_action();
                        }
                    });
                });
            });
    }

    /// 28 px icon-only ghost button with tooltip.
    fn refresh_button(&mut self, ui: &mut egui::Ui, tex: Option<&TextureHandle>) -> bool {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Refresh"));
        let hovered = resp.hovered();
        if hovered {
            ui.painter()
                .rect_filled(rect, Rounding::same(4.0), th::DEEP_RED);
        }
        if let Some(t) = tex {
            egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), Vec2::splat(16.0)))
                .paint_at(ui, rect.shrink(6.0));
        }
        resp.on_hover_text("Refresh connection, sync, staking and peer status")
            .clicked()
    }

    // -----------------------------------------------------------------------
    // Central work area (spec §5): CONTENT_BG, 24 px page padding, one page
    // owns the vertical scroll; max readable width 1120 px.
    // -----------------------------------------------------------------------
    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(th::CONTENT_BG)
                    .inner_margin(egui::Margin {
                        left: 24.0,
                        right: 24.0,
                        top: 24.0,
                        bottom: 24.0,
                    }),
            )
            .show(ctx, |ui| {
                if self.st.loaded() {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let w = ui.available_width().min(1120.0);
                            ui.allocate_ui(Vec2::new(w, ui.available_height()), |ui| {
                                // auto-update banner (non-intrusive; state machine
                                // in src/updater.rs decides when it shows)
                                self.update_banner(ui);
                                match self.st.screen {
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
                                }
                            });
                        });
                } else if self.st.seed_flow.is_some() {
                    self.seed_screen(ui);
                } else {
                    self.load_screen(ui);
                }
            });
    }

    // -----------------------------------------------------------------------
    // Seed-phrase wallet flows (create / restore) — state machine in
    // src/gui/seed_flow.rs, this is the thin view layer. Replaces the load
    // screen while active.
    // -----------------------------------------------------------------------
    fn seed_screen(&mut self, ui: &mut egui::Ui) {
        use crate::gui::seed_flow::SeedFlowStep as Step;
        // take() the flow out of AppState for the render so view closures can
        // reach self.st freely; restored below unless the flow ends itself.
        let mut flow = match self.st.seed_flow.take() {
            Some(f) => f,
            None => return,
        };
        let mut consumed = false;
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.add(egui::Label::new(
                egui::RichText::new("Seed phrase wallet")
                    .size(22.0)
                    .strong()
                    .color(th::TEXT_PRIMARY),
            ));
            th::text_meta(ui, "BIP39 + BIP32 — identical keys to the original qt wallet (m/44'/2222'/0'/0/i)");
            ui.add_space(8.0);
            th::inline_notice(ui, th::Notice::Info, "Please read", super::seed_flow::HONEST_NOTE);
            ui.add_space(10.0);
        });

        match flow.step.clone() {
            Step::Menu => {
                ui.vertical_centered(|ui| {
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if th::primary_button(ui, "Create new wallet (24 words)").clicked() {
                            flow.begin_create();
                        }
                        if ui.button("Restore from seed phrase").clicked() {
                            flow.begin_restore();
                        }
                        if ui.button("Back").clicked() {
                            consumed = true;
                        }
                    });
                    if let Some(e) = &flow.error {
                        ui.add_space(6.0);
                        th::inline_notice(ui, th::Notice::Error, "Could not start", e);
                    }
                });
            }
            Step::CreateReveal => {
                let words = flow.revealed_words().unwrap_or_default();
                ui.vertical_centered(|ui| {
                    th::inline_notice(
                        ui,
                        th::Notice::Warning,
                        "These 24 words are shown ONCE",
                        "Anyone who sees them can spend every coin this wallet will ever \
                         receive. Save them now — no one can recover them, not even tri7.",
                    );
                    ui.add_space(8.0);
                    ui.allocate_ui(Vec2::new(ui.available_width().min(760.0), 150.0), |ui| {
                        // selectable + copyable; right-click menu gives Select
                        // all/Copy on every platform (Windows paste-menu bar)
                        egui::TextEdit::multiline(&mut words.as_str())
                            .desired_rows(6)
                            .interactive(true)
                            .show(ui);
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Copy to clipboard").clicked() {
                            match arboard::Clipboard::new() {
                                Ok(mut cb) => {
                                    if let Err(e) = cb.set_text(words.clone()) {
                                        flow.error =
                                            Some(format!("clipboard copy failed: {}", e));
                                    }
                                }
                                Err(e) => {
                                    flow.error = Some(format!("clipboard unavailable: {}", e))
                                }
                            }
                        }
                        if ui.button("Save to file...").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_file_name("tri7-seed-backup.txt")
                                .set_title("Save your seed phrase backup")
                                .save_file()
                            {
                                if let Err(e) = std::fs::write(p, &words) {
                                    flow.error = Some(format!("save failed: {}", e));
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if ui
                        .checkbox(&mut flow.saved_confirmed, "I wrote the words down (paper or file) and stored them safely")
                        .changed()
                    {
                        flow.error = None;
                    }
                    ui.horizontal(|ui| {
                        if th::primary_button(ui, "I saved it — continue").clicked() {
                            flow.confirm_saved();
                        }
                        if ui.button("Cancel (discard these words)").clicked() {
                            consumed = true;
                        }
                    });
                    if let Some(e) = &flow.error {
                        ui.add_space(6.0);
                        th::inline_notice(ui, th::Notice::Error, "Hold on", e);
                    }
                });
            }
            Step::CreateConfirm => {
                ui.vertical_centered(|ui| {
                    th::inline_notice(
                        ui,
                        th::Notice::Info,
                        "Verify your backup",
                        "Type the 24 words in order (spaces or newlines). This proves the \
                         backup you saved is correct — without it, a bad backup is only \
                         discovered when the coins are already gone.",
                    );
                    ui.add_space(8.0);
                    ui.allocate_ui(Vec2::new(ui.available_width().min(760.0), 150.0), |ui| {
                        ui.allocate_ui(Vec2::new(760.0, 140.0), |ui| {
                            egui::TextEdit::multiline(&mut flow.words_input)
                                .desired_rows(6)
                                .desired_width(760.0)
                                .show(ui);
                        });
                    });
                    ui.horizontal(|ui| {
                        if th::primary_button(ui, "Verify words").clicked() {
                            flow.submit_create_confirm();
                        }
                        if ui.button("Cancel (discard this wallet)").clicked() {
                            consumed = true;
                        }
                    });
                    if let Some(e) = &flow.error {
                        ui.add_space(6.0);
                        th::inline_notice(ui, th::Notice::Error, "Words do not match", e);
                    }
                });
            }
            Step::RestoreInput => {
                ui.vertical_centered(|ui| {
                    th::inline_notice(
                        ui,
                        th::Notice::Info,
                        "Restore a wallet from its seed phrase",
                        "Paste or type the 12/24 words in order, then the optional \
                         passphrase (\"25th word\") if the wallet was created with one. \
                         The words must match EXACTLY — including the passphrase — or a \
                         different (empty) wallet is derived.",
                    );
                    ui.add_space(8.0);
                    ui.allocate_ui(Vec2::new(ui.available_width().min(760.0), 150.0), |ui| {
                        ui.allocate_ui(Vec2::new(760.0, 140.0), |ui| {
                            egui::TextEdit::multiline(&mut flow.words_input)
                                .desired_rows(6)
                                .desired_width(760.0)
                                .show(ui);
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Passphrase (optional):")
                                .color(th::TEXT_SECONDARY),
                        );
                        th::text_input(ui, 320.0, &mut flow.passphrase);
                    });
                    ui.horizontal(|ui| {
                        if th::primary_button(ui, "Validate phrase").clicked() {
                            flow.submit_restore();
                        }
                        if ui.button("Back").clicked() {
                            flow.abandon();
                        }
                    });
                    if let Some(e) = &flow.error {
                        ui.add_space(6.0);
                        th::inline_notice(ui, th::Notice::Error, "Invalid phrase", e);
                    }
                });
            }
            Step::Ready => {
                ui.vertical_centered(|ui| {
                    th::inline_notice(
                        ui,
                        th::Notice::Success,
                        "Phrase validated",
                        "The mnemonic checksum is valid. Choose the wallet file and how \
                         many addresses to derive, then write the wallet.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Wallet file:").color(th::TEXT_SECONDARY));
                        if th::text_input(ui, 420.0, &mut flow.wallet_path).changed() {
                            flow.error = None;
                        }
                        if ui.button("Browse...").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("Wallet files", &["dat"])
                                .set_title("Wallet file to create or restore into")
                                .save_file()
                            {
                                flow.wallet_path = p.display().to_string();
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Addresses to derive (first launch):")
                                .color(th::TEXT_SECONDARY),
                        );
                        let mut count = flow.key_count as i32;
                        if ui
                            .add(egui::DragValue::new(&mut count).range(1..=10000))
                            .changed()
                        {
                            flow.key_count = count.max(1) as usize;
                        }
                    });
                    th::text_meta(
                        ui,
                        "the original qt continues this sequence automatically via the hdchain counter",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if th::primary_button(ui, "Write wallet.dat").clicked() {
                            flow.error = None;
                            match flow.execute() {
                                Ok(_) => {
                                    // open what we just wrote
                                    self.st.cfg.wallet_path = flow.wallet_path.clone();
                                }
                                Err(e) => flow.error = Some(e),
                            }
                        }
                        if ui.button("Back").clicked() {
                            flow.abandon();
                        }
                    });
                    if let Some(e) = &flow.error {
                        ui.add_space(6.0);
                        th::inline_notice(ui, th::Notice::Error, "Could not write wallet", e);
                    }
                });
            }
            Step::Done(addresses) => {
                ui.vertical_centered(|ui| {
                    th::inline_notice(
                        ui,
                        th::Notice::Success,
                        "Wallet written and verified",
                        &format!(
                            "{} seed-derived addresses written in the exact legacy record \
                             format (the original qt wallet reads this file). The seed \
                             phrase is now your ONLY backup — keep it safe.",
                            addresses.len()
                        ),
                    );
                    if !addresses.is_empty() {
                        th::text_meta(ui, &format!("first address: {}", addresses[0]));
                        th::text_meta(ui, &format!("last address:  {}", addresses[addresses.len() - 1]));
                    }
                    ui.add_space(8.0);
                    if th::primary_button(ui, "Open this wallet").clicked() {
                        consumed = true;
                        if let Err(e) = self.st.load_wallet_action() {
                            self.st.load_error = Some(e);
                        } else {
                            self.st.refresh_debug_info_action();
                        }
                    }
                });
            }
        }
        if !consumed {
            self.st.seed_flow = Some(flow);
        }
    }

    // -----------------------------------------------------------------------
    // Auto-update banner (spec: "Cryptographic Triangles <ver> available —
    // Download & Install / Skip this version / View release notes"). Shows
    // ONLY when the pure state machine in src/updater.rs holds a banner
    // phase; never downloads or installs without the user's click.
    // -----------------------------------------------------------------------
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(line) = self.st.updater_banner_line() else {
            return;
        };
        let phase = self.st.updater.phase.clone();
        let (kind, title) = match &phase {
            UpdatePhaseKind::Available => (th::Notice::Info, line),
            UpdatePhaseKind::Downloading { .. } => (th::Notice::Info, line),
            UpdatePhaseKind::ReadyToInstall => (th::Notice::Success, line),
            UpdatePhaseKind::Failed(_) => (th::Notice::Error, line),
            _ => return,
        };
        th::inline_notice(ui, kind, &title, "");
        // release notes preview: the user sees WHAT they are getting before
        // any download button does anything
        let notes: Option<(String, String, String)> = self.st.updater.release.as_ref().map(|r| {
            (
                r.version.to_string_short(),
                r.html_url.clone(),
                r.body.lines().take(6).collect::<Vec<_>>().join("\n"),
            )
        });
        if let Some((ver, url, body)) = notes {
            if !body.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(body).size(12.0).color(th::TEXT_MUTED),
                    )
                    .wrap(),
                );
            }
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("v{ver}")).color(th::TEXT_SECONDARY),
                );
                if th::ghost_button(ui, "View release notes").clicked() {
                    self.st.updater_view_notes_action();
                }
                let _ = url;
            });
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            match &phase {
                UpdatePhaseKind::Available => {
                    if th::primary_button(ui, "Download & Install").clicked() {
                        if let Err(e) = self.st.updater_download_action() {
                            self.st.status_log.push(format!("update: {e}"));
                        }
                    }
                    if th::ghost_button(ui, "Skip this version").clicked() {
                        self.st.updater_skip_action();
                    }
                    if th::ghost_button(ui, "Dismiss").clicked() {
                        self.st.updater_dismiss_action();
                    }
                }
                UpdatePhaseKind::Downloading { .. } => {
                    ui.label(
                        egui::RichText::new("verifying checksum when done…")
                            .size(12.0)
                            .color(th::TEXT_MUTED),
                    );
                }
                UpdatePhaseKind::ReadyToInstall => {
                    if th::primary_button(ui, "Install now & restart").clicked() {
                        match self.st.updater_install_action() {
                            Ok(desc) => self.st.status_log.push(format!("update: {desc}")),
                            Err(e) => self.st.status_log.push(format!("update: {e}")),
                        }
                    }
                    if th::ghost_button(ui, "Skip this version").clicked() {
                        self.st.updater_skip_action();
                    }
                }
                UpdatePhaseKind::Failed(err) => {
                    if th::secondary_button(ui, "Retry").clicked() {
                        self.st.updater_retry_action();
                    }
                    if th::ghost_button(ui, "Dismiss").clicked() {
                        self.st.updater_dismiss_action();
                    }
                    let _ = err;
                }
                _ => {}
            }
        });
        ui.add_space(8.0);
    }
}

impl eframe::App for Tri7App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Self::apply_theme(ctx);
        self.header(ctx);
        self.sidebar(ctx);
        self.statusbar(ctx);
        self.central(ctx);

        self.dialogs(ctx);
        self.debug_window(ctx);
        if DEBUG_TOGGLE.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.console_open = true;
        }
        // live-panel polling: staking + peers every ~5s (cheap loopback RPCs)
        self.poll_ticker = self.poll_ticker.wrapping_add(1);
        if self.poll_ticker % 10 == 0 {
            self.st.staking_poll_action();
            self.st.peers_poll_action();
        }
        // auto-update: startup + every 6h auto-check, event drain (non-blocking)
        self.st.updater_tick();
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}


// ---------------------------------------------------------------------------
// Panels / pages (spec §10)
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
            ui.add_space(48.0);
            if let Some(tex) = self.tex(ui.ctx(), "brand/header") {
                ui.add(img(&tex, Vec2::new(371.0, 78.0))); // 1199x252 aspect-true
            }
            ui.add_space(14.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(s::APP_TITLE)
                        .size(25.0)
                        .strong()
                        .color(th::TEXT_PRIMARY),
                ),
            );
            th::text_meta(
                ui,
                "tri7 engine — wallet.dat opens READ-WRITE (New Address writes real legacy keys); signed txs broadcast via tri7d",
            );
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Wallet file:").color(th::TEXT_SECONDARY),
                );
                if th::text_input(ui, 420.0, &mut self.st.cfg.wallet_path).changed() {
                    self.st.load_error = None;
                }
                if ui.button("Browse...").clicked() {
                    if let Some(picked) =
                        rfd::FileDialog::new()
                            .add_filter("Wallet files", &["dat"])
                            .set_title("Choose wallet.dat")
                            .pick_file()
                    {
                        self.st.cfg.wallet_path = picked.display().to_string();
                        self.st.load_error = None;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("State folder:").color(th::TEXT_SECONDARY),
                );
                if th::text_input(ui, 420.0, &mut self.st.cfg.state_dir).changed() {
                    self.st.load_error = None;
                }
                if ui.button("Browse folder...").clicked() {
                    if let Some(picked) =
                        rfd::FileDialog::new()
                            .set_title("Choose the state folder")
                            .pick_folder()
                    {
                        self.st.cfg.state_dir = picked.display().to_string();
                        self.st.load_error = None;
                    }
                }
            });
            ui.add_space(8.0);
            if th::primary_button(ui, "Open Wallet").clicked() {
                if let Err(e) = self.st.load_wallet_action() {
                    self.st.load_error = Some(e);
                } else {
                    self.st.refresh_debug_info_action();
                }
            }
            if let Some(e) = &self.st.load_error {
                ui.add_space(4.0);
                th::inline_notice(ui, th::Notice::Error, "Could not open wallet", e);
            }
            ui.add_space(18.0);
            ui.separator();
            ui.add_space(6.0);
            th::text_meta(
                ui,
                "Seed phrase wallet — BIP39 create/restore, identical keys to the original qt (m/44'/2222'/0'/0/i)",
            );
            ui.add_space(4.0);
            if th::primary_button(ui, "Seed phrase wallet…").clicked() {
                let default_path = if self.st.cfg.wallet_path.trim().is_empty() {
                    "seed_wallet.dat".to_string()
                } else {
                    std::path::Path::new(&self.st.cfg.wallet_path)
                        .parent()
                        .map(|p| p.join("seed_wallet.dat").display().to_string())
                        .unwrap_or_else(|| "seed_wallet.dat".to_string())
                };
                self.st.seed_flow =
                    Some(crate::gui::seed_flow::SeedFlow::new(default_path));
            }
            ui.add_space(4.0);
            th::text_meta(
                ui,
                "existing wallets created with random keys have NO seed phrase — one can never be added to them",
            );
            ui.add_space(6.0);
            th::text_meta(
                ui,
                "key count + coverage height appear after load; default wallet: /root/.triangles/wallet.dat",
            );
        });
    }

    // -----------------------------------------------------------------------
    // §10.1 Overview
    // -----------------------------------------------------------------------
    fn page_overview(&mut self, ui: &mut egui::Ui) {
        th::page_header(ui, "Overview", "Wallet balance and recent activity");
        let avail = ui.available_width();
        if avail >= 900.0 {
            let left_w = (avail - 16.0) * 0.42;
            ui.horizontal(|ui| {
                ui.allocate_ui(Vec2::new(left_w, 200.0), |ui| self.overview_balance(ui));
                ui.allocate_ui(Vec2::new(avail - left_w - 16.0, 200.0), |ui| {
                    self.overview_recent(ui);
                });
            });
        } else {
            self.overview_balance(ui);
            ui.add_space(8.0);
            self.overview_recent(ui);
        }
        ui.add_space(8.0);
        self.overview_wallet_actions(ui);
        ui.add_space(8.0);
        self.overview_network_details(ui);
    }

    /// Balance card — the historical Wallet frame (Spendable/Stake/
    /// Unconfirmed/Immature → divider → Total) with its original tooltips.
    fn overview_balance(&mut self, ui: &mut egui::Ui) {
        let marker_tex = self.tex(ui.ctx(), "brand/mark16");
        let st = &mut self.st;
        th::card_titled(ui, "Wallet", |ui| {
            let Some(derived) = st.derived.clone() else {
                th::empty_state(ui, None, "No wallet loaded", "Open a wallet to see balances");
                return;
            };
            let total = derived.balances.total_sats;
            let tip = st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
            let mut immature = 0i64;
            for u in &derived.utxos {
                if !u.spendable_at(tip + 1) {
                    immature += u.value_sats;
                }
            }
            let (spendable, unconfirmed, stake) = (total - immature, 0i64, 0i64);
            let row = |ui: &mut egui::Ui, k: &str, tip: &str, v: &str, marker: bool| {
                ui.horizontal(|ui| {
                    if marker {
                        if let Some(tex) = &marker_tex {
                            ui.add(img(tex, Vec2::splat(14.0)));
                        }
                    }
                    ui.label(egui::RichText::new(k).color(th::TEXT_SECONDARY));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{} {}", v, s::TICKER))
                                .monospace()
                                .color(th::TEXT_PRIMARY),
                        )
                        .on_hover_text(tip);
                    });
                });
            };
            row(ui, "Spendable:", "Your current spendable balance", &s::format_tri(spendable), true);
            row(ui, "Stake:", "Total of coins that was staked, and do not yet count toward the current balance", &s::format_tri(stake), false);
            row(ui, "Unconfirmed:", "Total of transactions that have yet to be confirmed, and do not yet count toward the current balance", &s::format_tri(unconfirmed), false);
            row(ui, "Immature:", "Mined balance that has not yet matured", &s::format_tri(immature), false);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Total:").color(th::TEXT_SECONDARY));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{} {}", s::format_tri(total), s::TICKER))
                            .size(22.0)
                            .strong()
                            .monospace()
                            .color(th::TEXT_PRIMARY),
                    )
                    .on_hover_text("Your current total balance");
                });
            });
        });
        // snapshot/coverage/reconciliation sentence → collapsed "Data details"
        let st = &mut self.st;
        if let Some(derived) = &st.derived {
            egui::CollapsingHeader::new("Data details")
                .default_open(false)
                .show(ui, |ui| {
                    th::text_meta(
                        ui,
                        &format!(
                            "{} {} ({} sats) in {} UTXOs across {} addresses — coverage h{}{}",
                            derived.balances.total_sats as f64 / 1_000_000.0,
                            s::TICKER,
                            derived.balances.total_sats,
                            derived.utxos.len(),
                            derived.balances.per_address.iter().filter(|a| a.sats > 0).count(),
                            derived.coverage_height,
                            if derived.reconcile.enabled { ", live-tail reconciled" } else { ", snapshot" }
                        ),
                    );
                    if !derived.reconcile.note.is_empty() {
                        th::text_meta(ui, &derived.reconcile.note);
                    }
                });
        }
    }

    /// Recent transactions card with a View all action (spec §10.1).
    fn overview_recent(&mut self, ui: &mut egui::Ui) {
        let confirmed_tex = self.tex(ui.ctx(), "icons/tx_confirmed");
        let pending_tex = self.tex(ui.ctx(), "icons/clock1");
        let st = &mut self.st;
        th::card(ui, |ui| {
            ui.horizontal(|ui| {
                th::text_section(ui, "Recent transactions");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if th::ghost_button(ui, "View all").clicked() {
                        st.goto(Screen::Transactions);
                    }
                });
            });
            ui.add_space(6.0);
            let rows = recent_utxos(st, 8);
            if rows.is_empty() {
                th::empty_state(
                    ui,
                    None,
                    "No transactions yet",
                    "Incoming payments will appear here",
                );
                return;
            }
            let w = ui.available_width();
            let addr_w = (w - 24.0 - 150.0 - 120.0).max(120.0);
            let cols = [
                th::TableCol::new("", 24.0, false),
                th::TableCol::new("Time", 150.0, false),
                th::TableCol::new("Address", addr_w, false),
                th::TableCol::new("Amount", 120.0, true),
            ];
            th::table_header(ui, &cols, w);
            let tip_h = st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
            for (i, u) in rows.iter().enumerate() {
                let depth = tip_h.saturating_sub(u.height);
                let tex = if depth >= 6 { &confirmed_tex } else { &pending_tex };
                let color = if depth >= 6 {
                    th::TX_IN_GREEN_BRIGHT
                } else {
                    th::TX_IN_GREEN
                };
                th::table_row(ui, i, false, w, |rui| {
                    if let Some(tex) = tex {
                        egui::Image::from_texture(egui::load::SizedTexture::new(
                            tex.id(),
                            Vec2::splat(16.0),
                        ))
                        .paint_at(
                            rui,
                            egui::Rect::from_min_size(
                                egui::pos2(rui.max_rect().left() + 4.0, rui.max_rect().top() + 11.0),
                                Vec2::splat(16.0),
                            ),
                        );
                    }
                    th::table_cell_text(rui, 24.0, cols[1].width, &fmt_unix(u.tx_time), false, th::TEXT_SECONDARY, false);
                    th::table_cell_text(rui, 174.0, cols[2].width, &u.address, false, th::TEXT_PRIMARY, true);
                    th::table_cell_text(
                        rui,
                        174.0 + addr_w,
                        cols[3].width,
                        &format!("+{} {}", s::format_tri(u.value_sats), s::TICKER),
                        true,
                        color,
                        true,
                    );
                });
            }
        });
    }

    /// Wallet actions card (backup) — spec §10.1.
    fn overview_wallet_actions(&mut self, ui: &mut egui::Ui) {
        th::card_titled(ui, "Wallet actions", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Backup Wallet to:").color(th::TEXT_SECONDARY),
                );
                th::text_input(ui, 380.0, &mut self.st.dialogs.backup_path);
                if th::secondary_button(ui, "Back up wallet").clicked()
                    && !self.st.dialogs.backup_path.is_empty()
                {
                    let p = self.st.dialogs.backup_path.clone();
                    match self.st.backup_wallet_action(&p) {
                        Ok(n) => {
                            self.st.dialogs.backup =
                                Some(Ok(format!("wallet backed up to {} ({} bytes)", p, n)))
                        }
                        Err(e) => self.st.dialogs.backup = Some(Err(e)),
                    }
                }
                self.ico(ui, "menu_16/backup", 16.0);
            });
            match self.st.dialogs.backup.clone() {
                Some(Ok(m)) => {
                    ui.add_space(4.0);
                    th::inline_notice(ui, th::Notice::Success, "Backup complete", &m);
                }
                Some(Err(e)) => {
                    ui.add_space(4.0);
                    th::inline_notice(ui, th::Notice::Error, "Backup failed", &e);
                }
                None => {}
            }
        });
    }

    /// Network details disclosure with the live peers table — spec §10.1
    /// (collapsed by default; must not dominate the Overview).
    fn overview_network_details(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Network details")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if th::secondary_button(ui, "Refresh peers").clicked() {
                        self.st.peers_poll_action();
                    }
                    if let Some(e) = &self.st.peers_live.last_error {
                        th::inline_notice(ui, th::Notice::Error, "RPC error", e);
                    }
                });
                if let Some(table) = &self.st.peers_live.table {
                    if let Some(rows) = table.get("peers").and_then(|p| p.as_array()) {
                        let w = ui.available_width();
                        let fixed = 40.0 + 260.0 + 80.0 + 140.0 + 80.0;
                        let subver_w = (w - fixed).max(120.0);
                        let cols = [
                            th::TableCol::new("Dir", 40.0, false),
                            th::TableCol::new("Endpoint", 260.0, false),
                            th::TableCol::new("Transport", 80.0, false),
                            th::TableCol::new("Subver", subver_w, false),
                            th::TableCol::new("Msgs", 140.0, true),
                            th::TableCol::new("Traffic", 80.0, true),
                        ];
                        th::table_header(ui, &cols, w);
                        for (i, r) in rows.iter().enumerate() {
                            let transport =
                                r.get("transport").and_then(|x| x.as_str()).unwrap_or("?");
                            let tcol = match transport {
                                "tor" => Color32::from_rgb(0xaa, 0x77, 0xee),
                                "i2p" => th::INFO,
                                _ => th::TEXT_SECONDARY,
                            };
                            let x_subver = 380.0;
                            let x_msgs = x_subver + subver_w;
                            let x_traffic = x_msgs + 140.0;
                            th::table_row(ui, i, false, w, |rui| {
                                th::table_cell_text(
                                    rui, 0.0, cols[0].width,
                                    if r.get("inbound") == Some(&Value::Bool(true)) { "in" } else { "out" },
                                    false, th::TEXT_SECONDARY, false,
                                );
                                th::table_cell_text(
                                    rui, 40.0, cols[1].width,
                                    r.get("endpoint").and_then(|x| x.as_str()).unwrap_or("?"),
                                    false, th::TEXT_PRIMARY, true,
                                );
                                th::table_cell_text(rui, 300.0, cols[2].width, transport, false, tcol, false);
                                th::table_cell_text(
                                    rui, x_subver, cols[3].width,
                                    r.get("subver").and_then(|x| x.as_str()).unwrap_or("?"),
                                    false, th::TEXT_SECONDARY, false,
                                );
                                th::table_cell_text(
                                    rui, x_msgs, cols[4].width,
                                    &format!(
                                        "{}/{}",
                                        r.get("msgs_in").and_then(|x| x.as_u64()).unwrap_or(0),
                                        r.get("msgs_out").and_then(|x| x.as_u64()).unwrap_or(0)
                                    ),
                                    true, th::TEXT_SECONDARY, true,
                                );
                                th::table_cell_text(
                                    rui, x_traffic, cols[5].width,
                                    &format!(
                                        "{}ms ago",
                                        r.get("last_traffic_ms_ago")
                                            .and_then(|x| x.as_u64())
                                            .unwrap_or(0)
                                    ),
                                    true, th::TEXT_MUTED, true,
                                );
                            });
                        }
                    }
                }
            });
    }

    // -----------------------------------------------------------------------
    // §10.2 Send Coins — Details → Review signed transaction → Broadcast
    // -----------------------------------------------------------------------
    fn send_step_rail(&mut self, ui: &mut egui::Ui, step: SendStep) {
        let steps = [
            (SendStep::Details, "1", "Details"),
            (SendStep::Review, "2", "Review signed transaction"),
            (SendStep::Broadcast, "3", "Broadcast"),
        ];
        ui.horizontal(|ui| {
            for (i, (st_, num, label)) in steps.iter().enumerate() {
                let active = *st_ == step;
                let done = *st_ < step;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                let fill = if active || done { th::BRAND_RED } else { th::INPUT_BG };
                ui.painter().rect_filled(rect, Rounding::same(12.0), fill);
                let galley = ui.painter().layout_no_wrap(
                    (*num).to_string(),
                    egui::FontId::proportional(13.0),
                    Color32::BLACK,
                );
                ui.painter().galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    Color32::BLACK,
                );
                ui.label(
                    egui::RichText::new(*label)
                        .size(13.0)
                        .strong()
                        .color(if active {
                            th::TEXT_PRIMARY
                        } else {
                            th::TEXT_MUTED
                        }),
                );
                if i < 2 {
                    ui.label(egui::RichText::new("—").color(th::BORDER));
                }
            }
        });
    }

    fn page_send(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Send Coins",
            "Create, review, sign, and broadcast a Triangles transaction",
        );
        let step = match (&self.st.send_result, &self.st.broadcast.result, self.send_step) {
            (_, Some(_), _) if self.send_step == SendStep::Broadcast => SendStep::Broadcast,
            (Some(Ok(_)), _, SendStep::Broadcast) => SendStep::Broadcast,
            (Some(Ok(_)), _, _) => SendStep::Review,
            _ => SendStep::Details,
        };
        self.send_step_rail(ui, step);
        ui.add_space(8.0);

        // ---- Details card ----
        th::card_titled(ui, "Details", |ui| {
            let st = &mut self.st;
            egui::Grid::new("send_grid")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("Send from:").color(th::TEXT_SECONDARY),
                    );
                    let funded = st.funded_addresses();
                    let from = &mut st.send.from;
                    let sel = if from.is_empty() {
                        "select a funded address".to_string()
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
                    };
                    let from_resp = egui::ComboBox::from_id_source("from_addr")
                        .selected_text(sel)
                        .width(480.0)
                        .show_ui(ui, |ui| {
                            for (addr, bal) in &funded {
                                ui.selectable_value(
                                    from,
                                    addr.clone(),
                                    format!("{} — {} {}", addr, s::format_tri(*bal), s::TICKER),
                                );
                            }
                        });
                    from_resp.response.on_hover_text(
                        "Full-width funded-address combo — available balance shown right",
                    );
                    let avail_bal = funded
                        .iter()
                        .find(|(a, _)| *a == from.clone())
                        .map(|(_, v)| *v);
                    if let Some(av) = avail_bal {
                        ui.label(
                            egui::RichText::new(format!(
                                "available: {} {}",
                                s::format_tri(av),
                                s::TICKER
                            ))
                            .monospace()
                            .color(th::TEXT_SECONDARY),
                        );
                    } else {
                        ui.label("");
                    }
                    ui.end_row();

                    ui.label(egui::RichText::new("Pay to:").color(th::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        th::text_input(ui, 480.0, &mut st.send.dest);
                        // address-book picker (view-only; fills the dest field)
                        let book: Vec<String> = st
                            .all_book_rows()
                            .iter()
                            .map(|(a, _, _)| a.clone())
                            .collect();
                        let book_resp = egui::ComboBox::from_id_source("dest_book")
                            .selected_text("address book")
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for a in &book {
                                    let d = &mut st.send.dest;
                                    if ui.selectable_label(d != a, a.clone()).clicked() {
                                        *d = a.clone();
                                    }
                                }
                            });
                        book_resp
                            .response
                            .on_hover_text("Pick an address from the address book");
                    });
                    ui.end_row();

                    ui.label(egui::RichText::new("Amount:").color(th::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        th::text_input(ui, 180.0, &mut st.send.amount);
                        ui.label(
                            egui::RichText::new(format!("{} (6 decimals)", s::TICKER))
                                .color(th::TEXT_MUTED),
                        );
                    });
                    ui.end_row();

                    ui.label(egui::RichText::new("Label:").color(th::TEXT_SECONDARY));
                    th::disabled_input(
                        ui,
                        480.0,
                        "—",
                        "Recipient labels are not stored by this build yet",
                    );
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let resp = ui
                    .add_enabled(
                        false,
                        egui::Button::new(egui::RichText::new("Add recipient")),
                    );
                resp.on_disabled_hover_text(
                    "Multi-recipient sends are not supported by this build yet",
                );
            });
            // Fee + custom change under an Advanced disclosure
            egui::CollapsingHeader::new("Advanced")
                .default_open(false)
                .show(ui, |ui| {
                    let st = &mut self.st;
                    egui::Grid::new("send_adv_grid")
                        .num_columns(2)
                        .spacing([12.0, 12.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Fee:").color(th::TEXT_SECONDARY));
                            ui.horizontal(|ui| {
                                th::text_input(ui, 180.0, &mut st.send.fee);
                                ui.label(
                                    egui::RichText::new("(auto-raised to the legacy GetMinFee minimum)")
                                        .color(th::TEXT_MUTED),
                                );
                            });
                            ui.end_row();
                            ui.label(egui::RichText::new("Change:").color(th::TEXT_SECONDARY));
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut st.send.use_custom_change, "custom change address");
                                if st.send.use_custom_change {
                                    th::text_input(ui, 300.0, &mut st.send.custom_change);
                                }
                            });
                            ui.end_row();
                        });
                });
        });

        // ---- Coin Control Features (own collapsible card, default closed) ----
        if self.st.options.coin_control {
            self.send_coin_control(ui);
        }

        // ---- footer: local-signing note + primary action ----
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if let Some(tex) = self.tex(ui.ctx(), "menu_16/lock") {
                ui.add(img(&tex, Vec2::splat(16.0)));
            }
            ui.label(
                egui::RichText::new("Signing happens locally. Broadcasting uses your connected tri7d.")
                    .size(12.0)
                    .color(th::TEXT_SECONDARY),
            )
            .on_hover_text(
                "Signing is real (spend.rs, offline). Broadcast goes through YOUR tri7d's sendrawtransaction (consensus-validated, then relayed inv→getdata→tx).",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if th::primary_button_large(ui, "Review & sign").clicked() {
                    self.st.build_send_action(now_u32());
                    self.send_step = SendStep::Review;
                }
            });
        });
        ui.add_space(4.0);
        for line in self.st.status_log.iter().rev().take(3) {
            th::text_meta(ui, line);
        }

        // ---- Review signed transaction ----
        if let Some(Ok(b)) = self.st.send_result.clone() {
            self.send_review(ui, step, &b);
        } else if let Some(Err(e)) = &self.st.send_result {
            ui.add_space(8.0);
            th::inline_notice(ui, th::Notice::Error, "Could not build transaction", e);
        }
    }

    /// Coin Control Features card (coincontroldialog carry-over) — selection
    /// behavior identical to the previous view (indices into derived.utxos).
    fn send_coin_control(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Coin Control Features")
            .default_open(false)
            .show(ui, |ui| {
                let st = &mut self.st;
                let Some(derived) = st.derived.clone() else {
                    return;
                };
                ui.label(egui::RichText::new(if st.send.selected.is_empty() {
                    "Inputs... automatically selected"
                } else {
                    "Inputs... manually selected"
                })
                .color(th::TEXT_SECONDARY));
                let mut qty = 0usize;
                let mut amt = 0i64;
                let w = ui.available_width();
                let cols = [
                    th::TableCol::new("", 28.0, false),
                    th::TableCol::new("Transaction/Output", 220.0, false),
                    th::TableCol::new("Address/Label", (w - 28.0 - 220.0 - 120.0 - 120.0).max(120.0), false),
                    th::TableCol::new("Amount", 120.0, true),
                    th::TableCol::new("Height", 120.0, true),
                ];
                th::table_header(ui, &cols, w);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        let selected = st.send.selected.clone();
                        for (i, u) in derived.utxos.iter().enumerate() {
                            if u.address != st.send.from {
                                continue;
                            }
                            qty += 1;
                            amt += u.value_sats;
                            let on = selected.contains(&i);
                            let out_label =
                                format!("{}:{}", truncate_chars(&u.txid_display(), 16), u.n);
                            let addr = u.address.clone();
                            let amt_str = s::format_tri(u.value_sats);
                            let height = format!("h{}", u.height);
                            let idx = i;
                            th::table_row(ui, qty - 1, on, w, |rui| {
                                th::table_cell_widget(rui, 0.0, 28.0, |cui| {
                                    let mut on_mut = on;
                                    if cui.checkbox(&mut on_mut, "").changed() {
                                        let mut sel = st.send.selected.clone();
                                        if on_mut {
                                            if !sel.contains(&idx) {
                                                sel.push(idx);
                                                sel.sort_unstable();
                                            }
                                        } else {
                                            sel.retain(|x| *x != idx);
                                        }
                                        st.send.selected = sel;
                                    }
                                });
                                th::table_cell_text(
                                    rui, 28.0, cols[1].width, &out_label,
                                    false, th::TEXT_PRIMARY, true,
                                );
                                th::table_cell_text(
                                    rui, 248.0, cols[2].width, &addr,
                                    false, th::TEXT_SECONDARY, true,
                                );
                                let x_amt = 248.0 + cols[2].width;
                                th::table_cell_text(
                                    rui, x_amt, cols[3].width, &amt_str,
                                    true, th::TEXT_PRIMARY, true,
                                );
                                th::table_cell_text(
                                    rui, x_amt + 120.0, cols[4].width, &height,
                                    true, th::TEXT_SECONDARY, true,
                                );
                            });
                        }
                    });
                // legacy summary strip — unknown values stay "—"
                ui.add_space(6.0);
                let built = match &st.send_result {
                    Some(Ok(b)) => Some(b.clone()),
                    _ => None,
                };
                let summary = [
                    ("Quantity", qty.to_string()),
                    ("Bytes", built.as_ref().map(|b| b.n_bytes.to_string()).unwrap_or_else(|| "—".into())),
                    ("Amount", format!("{} {}", s::format_tri(amt), s::TICKER)),
                    ("Priority", "—".to_string()),
                    ("Fee", built.as_ref().map(|b| format!("{} {}", s::format_tri(b.fee_sats), s::TICKER)).unwrap_or_else(|| "—".into())),
                    ("Low Output", "—".to_string()),
                    ("After Fee", built.as_ref().map(|b| format!("{} {}", s::format_tri(amt - b.fee_sats), s::TICKER)).unwrap_or_else(|| "—".into())),
                    ("Change", built.as_ref().map(|b| format!("{} {}", s::format_tri(b.change_sats), s::TICKER)).unwrap_or_else(|| "—".into())),
                ];
                ui.horizontal_wrapped(|ui| {
                    for (k, v) in summary {
                        ui.label(egui::RichText::new(format!("{}:", k)).color(th::TEXT_SECONDARY));
                        ui.label(
                            egui::RichText::new(v).monospace().color(th::TEXT_PRIMARY),
                        );
                        ui.separator();
                    }
                });
                if th::ghost_button(ui, "Clear all").clicked() {
                    st.send.selected.clear();
                }
            });
    }

    /// Review card: two-column summary, raw hex collapsed, Back / Save /
    /// Broadcast (spec §10.2).
    fn send_review(&mut self, ui: &mut egui::Ui, step: SendStep, b: &s::SendBuild) {
        let busy = self.st.broadcast.busy;
        ui.add_space(12.0);
        th::card_titled(ui, "Review signed transaction", |ui| {
            let st = &mut self.st;
            egui::Grid::new("review_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Pay to:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(&st.send.dest).monospace().color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Amount:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {} (fee {} {})",
                            s::format_tri(b.amount_sats),
                            s::TICKER,
                            s::format_tri(b.fee_sats),
                            s::TICKER
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Change:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {}",
                            s::format_tri(b.change_sats),
                            s::TICKER
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Inputs:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!("{}", b.inputs.len()))
                            .monospace()
                            .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Size:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!("{} bytes", b.n_bytes))
                            .monospace()
                            .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("TxID:").color(th::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&b.txid_display)
                                .monospace()
                                .color(th::TEXT_PRIMARY),
                        );
                        if th::ghost_button(ui, "Copy TxID").clicked() {
                            ui.output_mut(|o| o.copied_text = b.txid_display.clone());
                        }
                    });
                    ui.end_row();
                });
            let raw = b.tx_hex.clone();
            egui::CollapsingHeader::new("Raw signed transaction")
                .default_open(false)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&raw).monospace().small().color(th::TEXT_SECONDARY),
                        )
                        .wrap(),
                    );
                    if th::ghost_button(ui, "Copy hex to clipboard").clicked() {
                        ui.output_mut(|o| o.copied_text = raw.clone());
                    }
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if th::secondary_button(ui, "Back").clicked() {
                    self.send_step = SendStep::Details;
                }
                // Save signed transaction (path + save_tx_hex_action)
                th::text_input_h(
                    ui,
                    300.0,
                    &mut st.dialogs.save_tx_path,
                    Some("path to save signed tx"),
                );
                if th::secondary_button(ui, "Save signed transaction").clicked()
                    && !st.dialogs.save_tx_path.is_empty()
                {
                    let p = st.dialogs.save_tx_path.clone();
                    match st.save_tx_hex_action(&p) {
                        Ok(n) => st
                            .status_log
                            .push(format!("saved signed tx to {} ({} bytes)", p, n)),
                        Err(e) => st.status_log.push(format!("save failed: {}", e)),
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let broadcast_clicked =
                        primary_button_enabled(ui, !busy, "Broadcast transaction").clicked();
                    if broadcast_clicked {
                        st.broadcast_action(b.tx_hex.clone());
                        self.send_step = SendStep::Broadcast;
                    }
                    if busy {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Broadcasting…")
                                .size(12.0)
                                .color(th::TEXT_SECONDARY),
                        );
                    }
                });
            });
            match &st.broadcast.result {
                Some(Ok(msg)) => {
                    ui.add_space(4.0);
                    th::inline_notice(ui, th::Notice::Success, "Broadcasted", msg);
                }
                Some(Err(e)) => {
                    ui.add_space(4.0);
                    // signed transaction is preserved; retry allowed
                    th::inline_notice(ui, th::Notice::Error, "Broadcast failed", e);
                }
                None => {}
            }
            let _ = step;
        });
    }
}

/// Scoped primary button usable with add_enabled (spec §7 Primary).
fn primary_button_enabled(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = th::BRAND_RED;
        v.inactive.bg_fill = th::BRAND_RED;
        v.inactive.bg_stroke = Stroke::NONE;
        v.inactive.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.hovered.weak_bg_fill = th::BRAND_RED_HOVER;
        v.hovered.bg_fill = th::BRAND_RED_HOVER;
        v.hovered.bg_stroke = Stroke::NONE;
        v.hovered.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.active.weak_bg_fill = th::BRAND_RED_PRESSED;
        v.active.bg_fill = th::BRAND_RED_PRESSED;
        v.active.bg_stroke = Stroke::NONE;
        v.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(label))
                .min_size(Vec2::new(0.0, 36.0)),
        )
    })
    .inner
}

/// Scoped secondary button usable with add_enabled (spec §7 Secondary).
fn secondary_button_enabled(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = th::INPUT_BG;
        v.inactive.bg_fill = th::INPUT_BG;
        v.inactive.bg_stroke = Stroke::new(1.0, th::BORDER);
        v.inactive.fg_stroke = Stroke::new(1.0, th::TEXT_PRIMARY);
        v.hovered.weak_bg_fill = th::DEEP_RED;
        v.hovered.bg_fill = th::DEEP_RED;
        v.hovered.bg_stroke = Stroke::new(1.0, th::BRAND_RED);
        v.hovered.fg_stroke = Stroke::new(1.0, th::TEXT_PRIMARY);
        v.active.weak_bg_fill = th::PRESSED_MAROON;
        v.active.bg_fill = th::PRESSED_MAROON;
        v.active.bg_stroke = Stroke::new(1.0, th::BRAND_RED);
        v.active.fg_stroke = Stroke::new(1.0, th::TEXT_PRIMARY);
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(label))
                .min_size(Vec2::new(0.0, 36.0)),
        )
    })
    .inner
}

impl Tri7App {
    // -----------------------------------------------------------------------
    // §10.3 Receive Coins — addresses left, payment request + inline QR right
    // -----------------------------------------------------------------------
    fn page_receive(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Receive Coins",
            "Manage receiving addresses and create payment requests",
        );
        // HD seed status strip: what this wallet's keys derive from (or the
        // honest no-seed note for pre-HD random-key wallets).
        let hd_line = match (
            self.st.wallet.as_ref().map(|w| w.hd_mnemonic.clone()).unwrap_or(None),
            self.st.wallet.as_ref().map(|w| w.hd_chain_index).unwrap_or(0),
        ) {
            (Some(_), next) => Some(format!(
                "Seed-derived wallet — new addresses derive at m/44'/2222'/0'/0/{} from the stored phrase (passphrase: {}).",
                next,
                self.st
                    .wallet
                    .as_ref()
                    .map(|w| if w.hd_passphrase.is_some() { "set" } else { "none" })
                    .unwrap_or("none")
            )),
            (None, _) if self.st.loaded() => None,
            _ => None,
        };
        if let Some(line) = hd_line {
            th::inline_notice(ui, th::Notice::Info, "HD wallet", &line);
            ui.add_space(4.0);
        } else if self.st.loaded() {
            th::inline_notice(
                ui,
                th::Notice::Info,
                "No seed phrase",
                "This wallet's keys were created randomly — it has no seed phrase and one \
                 can never be added. Back this wallet up by copying wallet.dat itself. \
                 New seed-phrase wallets are created from the load screen.",
            );
            ui.add_space(4.0);
        }
        let avail = ui.available_width();
        if avail >= 900.0 {
            let left_w = (avail - 16.0) * 0.60;
            ui.horizontal(|ui| {
                ui.allocate_ui(Vec2::new(left_w, 200.0), |ui| self.receive_addresses(ui));
                ui.allocate_ui(Vec2::new(avail - left_w - 16.0, 200.0), |ui| {
                    self.receive_request(ui);
                });
            });
        } else {
            self.receive_addresses(ui);
            ui.add_space(8.0);
            self.receive_request(ui);
        }
    }

    fn receive_addresses(&mut self, ui: &mut egui::Ui) {
        let copy_icon = self.tex(ui.ctx(), "menu_16/copy");
        th::card(ui, |ui| {
            // toolbar
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("New Address label:").color(th::TEXT_SECONDARY),
                );
                th::text_input(ui, 220.0, &mut self.st.receive.new_address_label);
                if th::primary_button(ui, "New address").clicked() {
                    let label = self.st.receive.new_address_label.clone();
                    match self.st.new_address_action(label) {
                        Ok(addr) => {
                            self.st.receive.address = addr;
                            self.st.refresh_receive_qr();
                        }
                        Err(e) => self.st.receive.error = Some(e),
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if th::secondary_button(ui, "Copy Address").clicked() {
                    ui.output_mut(|o| o.copied_text = self.st.receive.address.clone());
                }
                if th::ghost_button(ui, "Edit Label").clicked() {
                    let addr = self.st.receive.address.clone();
                    let draft = self.st.label_of(&addr).unwrap_or_default().to_string();
                    self.ab_edit = Some((addr, draft));
                }
                if let Some(tex) = &copy_icon {
                    ui.add(img(tex, Vec2::splat(16.0)));
                }
            });
            ui.add_space(8.0);
            // writability callout — only when unavailable (spec §10.3)
            if !self.st.wallet_writable() {
                th::inline_notice(
                    ui,
                    th::Notice::Info,
                    "wallet.dat is not writable this session",
                    "new keys go to the GUI keystore (see status log)",
                );
                ui.add_space(4.0);
            }
            let addrs = self.st.receive_addresses();
            let labels: Vec<(String, String, &'static str)> = addrs
                .iter()
                .map(|(addr, label, source)| {
                    let lab = self
                        .st
                        .label_of(addr)
                        .or_else(|| label.clone())
                        .unwrap_or_default();
                    (addr.clone(), lab, *source)
                })
                .collect();
            let sel = self.st.receive.address.clone();
            let w = ui.available_width();
            let label_w = (w * 0.35).max(120.0);
            let cols = [
                th::TableCol::new("Label", label_w, false),
                th::TableCol::new("Address", w - label_w - 110.0, false),
                th::TableCol::new("Source", 110.0, false),
            ];
            th::table_header(ui, &cols, w);
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for (i, (addr, lab, source)) in labels.iter().enumerate() {
                        let selected = sel == *addr;
                        let addr2 = addr.clone();
                        let lab2 = lab.clone();
                        let src2 = *source;
                        let ir = th::table_row(ui, i, selected, w, |rui| {
                            let resp = th::row_click(rui, 38.0);
                            th::table_cell_text(rui, 0.0, cols[0].width, &lab2, false, th::TEXT_PRIMARY, false);
                            th::table_cell_text(rui, label_w, cols[1].width, &addr2, false, th::TEXT_SECONDARY, true);
                            th::table_cell_text(
                                rui,
                                label_w + cols[1].width,
                                cols[2].width,
                                if src2 == "keystore" { "GUI keystore" } else { "wallet.dat" },
                                false,
                                th::TEXT_MUTED,
                                false,
                            );
                            resp.clicked()
                        });
                        if ir.inner {
                            self.st.receive.address = addr.clone();
                            self.st.refresh_receive_qr();
                        }
                    }
                });
        });
    }

    fn receive_request(&mut self, ui: &mut egui::Ui) {
        let qr_tex = self.qr_texture(ui.ctx());
        let st = &mut self.st;
        th::card_titled(ui, "Payment request", |ui| {
            if st.receive.address.is_empty() {
                th::empty_state(
                    ui,
                    None,
                    "No address selected",
                    "Select a receiving address on the left",
                );
                return;
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Address:").color(th::TEXT_SECONDARY));
                ui.label(
                    egui::RichText::new(&st.receive.address)
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                );
                if th::ghost_button(ui, "Copy").clicked() {
                    ui.output_mut(|o| o.copied_text = st.receive.address.clone());
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("qrform")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Label:").color(th::TEXT_SECONDARY));
                    th::text_input(ui, 260.0, &mut st.receive.label);
                    ui.end_row();
                    ui.label(egui::RichText::new("Message:").color(th::TEXT_SECONDARY));
                    th::text_input(ui, 260.0, &mut st.receive.message);
                    ui.end_row();
                    ui.label(egui::RichText::new("Amount:").color(th::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        th::text_input(ui, 160.0, &mut st.receive.amount);
                        ui.checkbox(
                            &mut st.receive.require_payment,
                            "Request a specific amount",
                        );
                    });
                    ui.end_row();
                });
            ui.add_space(4.0);
            // inline crisp QR as soon as the request is valid (spec §10.3)
            if let Some(tex) = qr_tex.as_ref() {
                egui::Frame::none()
                    .fill(Color32::WHITE) // mandatory white quiet-zone tile
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.add(img(tex, Vec2::splat(220.0)));
                    });
            }
            if !st.receive.uri.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&st.receive.uri)
                            .monospace()
                            .color(th::TEXT_PRIMARY),
                    )
                    .wrap(),
                );
                ui.horizontal(|ui| {
                    if th::secondary_button(ui, "Copy URI").clicked() {
                        ui.output_mut(|o| o.copied_text = st.receive.uri.clone());
                    }
                    th::text_input_h(
                        ui,
                        220.0,
                        &mut st.dialogs.qr_save_path,
                        Some("path to save QR PNG"),
                    );
                    if th::secondary_button(ui, "Save QR").clicked()
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
                    if th::ghost_button(ui, "Enlarge").clicked() {
                        st.dialogs.qr = true; // enlarged preview modal
                    }
                });
            } else {
                th::text_meta(
                    ui,
                    "QR appears here when the address and request fields are valid",
                );
            }
            if let Some(err) = &st.receive.error {
                ui.add_space(4.0);
                th::inline_notice(ui, th::Notice::Error, "Error", err);
            }
        });
    }

    // -----------------------------------------------------------------------
    // §10.4 Address Book
    // -----------------------------------------------------------------------
    fn page_addressbook(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Address Book",
            "Labels and addresses used by this wallet",
        );
        // edit dialog (selection/double-click opens it — no inline writes)
        self.ab_edit_dialog(ui);
        // delete confirmation
        self.ab_delete_confirm(ui);

        th::card(ui, |ui| {
            // toolbar: search, source filter, new address
            ui.horizontal(|ui| {
                th::text_input_h(
                    ui,
                    260.0,
                    &mut self.ab_search,
                    Some("search label or address"),
                );
                let sources = ["All", "wallet.dat", "GUI keystore", "Address book"];
                egui::ComboBox::from_id_source("ab_source")
                    .selected_text(sources[self.ab_source_filter])
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for (i, srcx) in sources.iter().enumerate() {
                            ui.selectable_value(&mut self.ab_source_filter, i, *srcx);
                        }
                    });
                ui.label(
                    egui::RichText::new("New address label:").color(th::TEXT_SECONDARY),
                );
                th::text_input(ui, 180.0, &mut self.ab_new_label);
                if th::primary_button(ui, "New address").clicked() {
                    let label = self.ab_new_label.clone();
                    match self.st.new_address_action(label) {
                        Ok(addr) => {
                            self.st.receive.address = addr;
                            self.st.refresh_receive_qr();
                        }
                        Err(e) => self.st.receive.error = Some(e),
                    }
                }
            });
            ui.add_space(8.0);
            let rows = self.st.all_book_rows();
            let search = self.ab_search.to_lowercase();
            let srcf = self.ab_source_filter;
            let sources = ["All", "wallet.dat", "GUI keystore", "Address book"];
            let filtered: Vec<(String, Option<String>, &'static str)> = rows
                .into_iter()
                .filter(|(addr, label, source)| {
                    let src_name = match *source {
                        "wallet" => "wallet.dat",
                        "keystore" => "GUI keystore",
                        _ => "address book",
                    };
                    if srcf != 0 && src_name != sources[srcf] {
                        return false;
                    }
                    if search.is_empty() {
                        return true;
                    }
                    addr.to_lowercase().contains(&search)
                        || label
                            .as_deref()
                            .map(|l| l.to_lowercase().contains(&search))
                            .unwrap_or(false)
                })
                .collect();
            let w = ui.available_width();
            let label_w = (w * 0.28).max(120.0);
            let addr_w = (w - label_w - 110.0 - 330.0).max(160.0);
            let cols = [
                th::TableCol::new("Label", label_w, false),
                th::TableCol::new("Address", addr_w, false),
                th::TableCol::new("Source", 110.0, false),
                th::TableCol::new("Actions", 330.0, true),
            ];
            th::table_header(ui, &cols, w);
            if filtered.is_empty() {
                th::empty_state(ui, None, "No addresses", "Add one with New address");
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                let writable = self.st.wallet_writable();
                for (i, (addr, label, source)) in filtered.iter().enumerate() {
                    let selected = self.ab_selected.as_deref() == Some(addr.as_str());
                    let editable = *source != "wallet" || writable;
                    let addr_s = addr.clone();
                    let lab = label.clone().unwrap_or_default();
                    let src_name = match *source {
                        "wallet" => "wallet.dat",
                        "keystore" => "GUI keystore",
                        _ => "address book",
                    };
                    let ir = th::table_row(ui, i, selected, w, |rui| {
                        let resp = th::row_click(rui, 38.0);
                        th::table_cell_text(rui, 0.0, cols[0].width, &lab, false, th::TEXT_PRIMARY, false);
                        th::table_cell_text(rui, label_w, cols[1].width, &addr_s, false, th::TEXT_SECONDARY, true);
                        th::table_cell_text(rui, label_w + addr_w, cols[2].width, src_name, false, th::TEXT_MUTED, false);
                        (resp.clicked(), resp.double_clicked())
                    });
                    let (clicked, double_clicked) = ir.inner;
                    if clicked || double_clicked {
                        self.ab_selected = Some(addr.clone());
                        if double_clicked && editable {
                            let draft = label.clone().unwrap_or_default();
                            self.ab_edit = Some((addr.clone(), draft));
                        }
                    }
                    // row actions
                    ui.horizontal(|ui| {
                        if th::ghost_button(ui, "Copy Address").clicked() {
                            ui.output_mut(|o| o.copied_text = addr_s.clone());
                        }
                        let edit = th::ghost_button(ui, "Edit Label");
                        if edit.clicked() && editable {
                            let draft = label.clone().unwrap_or_default();
                            self.ab_edit = Some((addr_s.clone(), draft));
                        }
                        if !editable {
                            edit.on_disabled_hover_text(
                                "wallet rows are read-only this session (wallet.dat not writable)",
                            );
                        }
                        if th::ghost_button(ui, "Sign Message").clicked() {
                            self.st.goto(Screen::SignMessage);
                        }
                        if th::ghost_button(ui, "Verify Message").clicked() {
                            self.st.goto(Screen::VerifyMessage);
                        }
                        let del = th::danger_button(ui, "Delete");
                        if del.clicked() && editable {
                            self.ab_delete = Some(addr_s.clone());
                        }
                        if !editable {
                            del.on_disabled_hover_text(
                                "wallet rows are read-only this session (wallet.dat not writable)",
                            );
                        }
                    });
                }
            });
        });
    }

    fn ab_edit_dialog(&mut self, ui: &mut egui::Ui) {
        // rendered as an inline modal panel over the page
        if let Some((addr, mut draft)) = self.ab_edit.clone() {
            let resp = ui.allocate_ui(Vec2::new(420.0, 130.0), |ui| {
                egui::Frame::none()
                    .fill(th::PANEL_BG)
                    .stroke(Stroke::new(1.0, th::BORDER))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(20.0))
                    .show(ui, |ui| {
                        th::text_section(ui, "Edit Label");
                        th::text_meta(ui, &addr);
                        ui.add_space(4.0);
                        th::text_input(ui, 360.0, &mut draft);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if th::secondary_button(ui, "Cancel").clicked() {
                                self.ab_edit = None;
                            }
                            if th::primary_button(ui, "Save").clicked() {
                                if let Err(e) = self.st.set_label_action(&addr, draft.clone()) {
                                    self.st.status_log.push(format!("label: {}", e));
                                }
                                self.ab_edit = None;
                            }
                        });
                    });
            });
            let _ = resp;
        }
    }

    fn ab_delete_confirm(&mut self, ui: &mut egui::Ui) {
        if let Some(addr) = self.ab_delete.clone() {
            ui.add_space(8.0);
            egui::Frame::none()
                .fill(th::PANEL_BG)
                .stroke(Stroke::new(1.0, th::ERROR))
                .rounding(Rounding::same(8.0))
                .inner_margin(egui::Margin::same(20.0))
                .show(ui, |ui| {
                    th::text_section(ui, "Delete address book entry?");
                    th::text_meta(ui, &format!("{} will be removed from the GUI address book", addr));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if th::secondary_button(ui, "Cancel").clicked() {
                            self.ab_delete = None;
                        }
                        if th::danger_button(ui, "Delete").clicked() {
                            if let Err(e) = self.st.delete_ab_entry_action(&addr) {
                                self.st.status_log.push(format!("delete: {}", e));
                            }
                            self.ab_delete = None;
                        }
                    });
                });
        }
    }

    // -----------------------------------------------------------------------
    // §10.5 Transactions
    // -----------------------------------------------------------------------
    fn tx_type_name(prev_kind: &str) -> &'static str {
        if prev_kind == "coinbase" {
            "Mined"
        } else if prev_kind == "coinstake" {
            "Stake reward"
        } else {
            "Received with"
        }
    }

    fn tx_type_index(prev_kind: &str) -> usize {
        if prev_kind == "coinbase" {
            2
        } else if prev_kind == "coinstake" {
            3
        } else {
            1
        }
    }

    fn page_transactions(&mut self, ui: &mut egui::Ui) {
        th::page_header(ui, "Transactions", "History for this wallet");
        let confirmed_tex = self.tex(ui.ctx(), "icons/tx_confirmed");
        let pending_tex = self.tex(ui.ctx(), "icons/clock1");
        th::card(ui, |ui| {
            // filter toolbar (historical page model: type/status/amount/search)
            ui.horizontal(|ui| {
                let types = ["All types", "Received with", "Mined", "Stake reward"];
                egui::ComboBox::from_id_source("tx_type")
                    .selected_text(types[self.tx_type_filter])
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for (i, tx_) in types.iter().enumerate() {
                            ui.selectable_value(&mut self.tx_type_filter, i, *tx_);
                        }
                    });
                let statuses = ["All statuses", "Confirmed", "Pending"];
                egui::ComboBox::from_id_source("tx_status")
                    .selected_text(statuses[self.tx_status_filter])
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (i, stx) in statuses.iter().enumerate() {
                            ui.selectable_value(&mut self.tx_status_filter, i, *stx);
                        }
                    });
                th::text_input_h(ui, 110.0, &mut self.tx_min_amount, Some("min amount"));
                th::text_input_h(
                    ui,
                    220.0,
                    &mut self.tx_search,
                    Some("label / address / txid"),
                );
            });
            ui.add_space(4.0);
            th::text_meta(
                ui,
                "wallet outputs currently unspent (credits) — full sent/history view lands with daemon integration",
            );
            ui.add_space(6.0);
            let Some(derived) = self.st.derived.clone() else {
                th::empty_state(
                    ui,
                    None,
                    "No wallet loaded",
                    "Open a wallet to see transactions",
                );
                return;
            };
            let tip = self.st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
            let min_amount_sats: i64 = self
                .tx_min_amount
                .trim()
                .parse::<f64>()
                .ok()
                .map(|v| (v * 1_000_000.0).round() as i64)
                .unwrap_or(0);
            let search = self.tx_search.to_lowercase();
            // newest first (existing order), then view filters
            let mut rows: Vec<(usize, &UtxoRec)> = derived
                .utxos
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, u)| {
                    if min_amount_sats > 0 && u.value_sats < min_amount_sats {
                        return false;
                    }
                    if self.tx_type_filter != 0
                        && Self::tx_type_index(&u.prev_kind) != self.tx_type_filter
                    {
                        return false;
                    }
                    let depth = tip.saturating_sub(u.height);
                    if self.tx_status_filter == 1 && depth < 6 {
                        return false;
                    }
                    if self.tx_status_filter == 2 && depth >= 6 {
                        return false;
                    }
                    if !search.is_empty() {
                        let label_hit = self
                            .st
                            .label_of(&u.address)
                            .map(|l| l.to_lowercase().contains(&search))
                            .unwrap_or(false);
                        if !label_hit
                            && !u.address.to_lowercase().contains(&search)
                            && !u.txid_display().to_lowercase().contains(&search)
                        {
                            return false;
                        }
                    }
                    true
                })
                .collect();
            let _ = &mut rows;
            let w = ui.available_width();
            let addr_w = (w - 24.0 - 150.0 - 110.0 - 110.0 - 100.0).max(140.0);
            let cols = [
                th::TableCol::new("", 24.0, false),
                th::TableCol::new("Date", 150.0, false),
                th::TableCol::new("Type", 110.0, false),
                th::TableCol::new("Label/Address", addr_w, false),
                th::TableCol::new("Amount", 110.0, true),
                th::TableCol::new("Confs", 100.0, true),
            ];
            th::table_header(ui, &cols, w);
            if rows.is_empty() {
                th::empty_state(
                    ui,
                    None,
                    "No matching transactions",
                    "Adjust the filters or load a wallet",
                );
            }
            let sel_idx = self.tx_selected;
            for (i, (orig_idx, u)) in rows.iter().enumerate() {
                let depth = tip.saturating_sub(u.height);
                let confirmed = depth >= 6;
                let tex = if confirmed {
                    confirmed_tex.as_ref()
                } else {
                    pending_tex.as_ref()
                };
                let color = if confirmed {
                    th::TX_IN_GREEN_BRIGHT
                } else {
                    th::TX_IN_GREEN
                };
                let type_label = Self::tx_type_name(&u.prev_kind);
                let label = self
                    .st
                    .label_of(&u.address)
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| u.address.clone());
                let amt = format!("+{} {}", s::format_tri(u.value_sats), s::TICKER);
                let conf_label = format!("{}", depth);
                let date = fmt_unix(u.tx_time);
                let selected = sel_idx == Some(*orig_idx);
                let ir = th::table_row(ui, i, selected, w, |rui| {
                    let resp = th::row_click(rui, 38.0);
                    if let Some(t) = tex {
                        egui::Image::from_texture(egui::load::SizedTexture::new(
                            t.id(),
                            Vec2::splat(16.0),
                        ))
                        .paint_at(
                            rui,
                            egui::Rect::from_min_size(
                                egui::pos2(rui.max_rect().left() + 4.0, rui.max_rect().top() + 11.0),
                                Vec2::splat(16.0),
                            ),
                        );
                    }
                    th::table_cell_text(rui, 24.0, cols[1].width, &date, false, th::TEXT_SECONDARY, false);
                    th::table_cell_text(rui, 174.0, cols[2].width, type_label, false, th::TEXT_PRIMARY, false);
                    th::table_cell_text(rui, 284.0, cols[3].width, &label, false, th::TEXT_PRIMARY, true);
                    th::table_cell_text(rui, 284.0 + addr_w, cols[4].width, &amt, true, color, true);
                    th::table_cell_text(
                        rui,
                        284.0 + addr_w + 110.0,
                        cols[5].width,
                        &conf_label,
                        true,
                        th::TEXT_SECONDARY,
                        true,
                    );
                    resp.clicked()
                });
                if ir.inner {
                    self.tx_selected = Some(*orig_idx);
                }
            }
        });
        // details drawer card below the table (single-column page)
        if let Some(idx) = self.tx_selected {
            self.tx_details(ui, idx);
        }
    }

    fn tx_details(&mut self, ui: &mut egui::Ui, idx: usize) {
        let Some(derived) = self.st.derived.clone() else {
            return;
        };
        let Some(u) = derived.utxos.get(idx) else {
            self.tx_selected = None;
            return;
        };
        let tip = self.st.tip.as_ref().map(|x| x.height as u32).unwrap_or(0);
        let depth = tip.saturating_sub(u.height);
        let confirmed = depth >= 6;
        let txid = u.txid_display().clone();
        let label = self
            .st
            .label_of(&u.address)
            .map(|l| l.to_string())
            .unwrap_or_default();
        ui.add_space(8.0);
        th::card_titled(ui, "Transaction details", |ui| {
            egui::Grid::new("tx_detail_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("TxID:").color(th::TEXT_SECONDARY));
                    ui.label(egui::RichText::new(&txid).monospace().color(th::TEXT_PRIMARY));
                    ui.end_row();
                    ui.label(egui::RichText::new("Status:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(if confirmed { "Confirmed" } else { "Pending" })
                            .color(if confirmed { th::SUCCESS } else { th::WARNING }),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Confirmations:").color(th::TEXT_SECONDARY));
                    ui.label(egui::RichText::new(depth.to_string()).monospace());
                    ui.end_row();
                    ui.label(egui::RichText::new("Date:").color(th::TEXT_SECONDARY));
                    ui.label(fmt_unix(u.tx_time));
                    ui.end_row();
                    ui.label(egui::RichText::new("Type:").color(th::TEXT_SECONDARY));
                    ui.label(Self::tx_type_name(&u.prev_kind));
                    ui.end_row();
                    ui.label(egui::RichText::new("Address:").color(th::TEXT_SECONDARY));
                    ui.label(egui::RichText::new(&u.address).monospace());
                    ui.end_row();
                    if !label.is_empty() {
                        ui.label(egui::RichText::new("Label:").color(th::TEXT_SECONDARY));
                        ui.label(&label);
                        ui.end_row();
                    }
                    ui.label(egui::RichText::new("Amount:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "+{} {}",
                            s::format_tri(u.value_sats),
                            s::TICKER
                        ))
                        .monospace()
                        .color(th::TX_IN_GREEN_BRIGHT),
                    );
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if th::secondary_button(ui, "Copy TxID").clicked() {
                    ui.output_mut(|o| o.copied_text = txid.clone());
                }
                if th::secondary_button(ui, "Open in Block Explorer").clicked() {
                    if self.st.explorer.fetch_tx(&txid).is_ok() {
                        self.st.explorer.open_tx = Some(txid.clone());
                        self.st.explorer.view = ex::ExplorerView::Tx;
                        self.ex_tab = ExTab::Transaction;
                        self.st.goto(Screen::Explorer);
                    } else {
                        self.st.explorer.last_error = Some("tx fetch failed".into());
                    }
                }
                if th::ghost_button(ui, "Close").clicked() {
                    self.tx_selected = None;
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // §10.6 Staking
    // -----------------------------------------------------------------------
    fn page_staking(&mut self, ui: &mut egui::Ui) {
        th::page_header(ui, "Staking", "Proof-of-stake status and wallet weight");
        let big_icon = self.tex(ui.ctx(), "icons/staking_off");
        let on_icon = self.tex(ui.ctx(), "icons/staking_on");
        let st = &mut self.st;
        let live = st.staking_live.clone();
        let enabled = live.field("enabled");
        let staking_on = enabled == "true" && live.field("staking") == "true";
        // ---- top status card ----
        th::card(ui, |ui| {
            ui.horizontal(|ui| {
                let tex = if staking_on { on_icon.as_ref() } else { big_icon.as_ref() };
                if let Some(t) = tex {
                    ui.add(img(t, Vec2::splat(48.0)));
                }
                ui.vertical(|ui| {
                    let state_line = if staking_on {
                        ("Staking active", th::SUCCESS)
                    } else if enabled == "true" {
                        ("Ready but not currently staking", th::WARNING)
                    } else {
                        ("Staking off", th::TEXT_MUTED)
                    };
                    ui.label(
                        egui::RichText::new(state_line.0)
                            .size(17.0)
                            .strong()
                            .color(state_line.1),
                    );
                    th::text_meta(
                        ui,
                        &format!(
                            "Staking loop: {}",
                            if staking_on {
                                "ON — searching kernels every tick"
                            } else if enabled == "true" {
                                "enabled but idle (no mature weight yet)"
                            } else {
                                "OFF (stake=0)"
                            }
                        ),
                    );
                    if let Some(lp) = &live.last_poll {
                        th::text_meta(ui, &format!("Last updated: {}", lp));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if th::secondary_button(ui, "Refresh (getstakinginfo)").clicked() {
                        st.staking_poll_action();
                    }
                    if enabled == "true" {
                        if th::danger_button(ui, "Staking OFF").clicked() {
                            st.staking_toggle_action(false);
                        }
                    } else if th::primary_button(ui, "Staking ON").clicked() {
                        st.staking_toggle_action(true);
                    }
                });
            });
            if let Some(e) = &st.staking_live.last_error {
                ui.add_space(4.0);
                th::inline_notice(ui, th::Notice::Error, "RPC error", e);
            }
        });
        ui.add_space(8.0);
        // ---- metric cards ----
        let young_n = match &st.derived {
            Some(d) => {
                let tip_h = st.tip.as_ref().map(|t| t.height).unwrap_or(0);
                d.utxos
                    .iter()
                    .filter(|u| tip_h.saturating_sub(u.height as u64) < 60)
                    .count()
            }
            None => 0,
        };
        ui.horizontal(|ui| {
            let metric = |ui: &mut egui::Ui, k: &str, v: &str| {
                th::card(ui, |ui| {
                    th::text_meta(ui, k);
                    ui.label(
                        egui::RichText::new(v)
                            .monospace()
                            .size(15.0)
                            .strong()
                            .color(th::TEXT_PRIMARY),
                    );
                });
            };
            let (spendable, utxo_n) = match &st.derived {
                Some(d) => (d.utxos.iter().map(|u| u.value_sats).sum::<i64>(), d.utxos.len()),
                None => (0, 0),
            };
            metric(ui, "Wallet weight", &live.field("weight"));
            metric(ui, "Network weight", &live.field("netstakeweight"));
            metric(ui, "Expected time", &format!("{}s", live.field("expectedtime")));
            metric(
                ui,
                "Mature / eligible",
                &format!(
                    "{} coins ({} young <1h)",
                    live.field("coins_meeting_min_age"),
                    young_n
                ),
            );
            metric(ui, "Reserve", "—");
            let _ = (spendable, utxo_n);
        });
        ui.add_space(8.0);
        // ---- eligibility / explanation panel ----
        let mut open_options = false;
        th::card_titled(ui, "Eligibility and diagnostics", |ui| {
            let Some(stake) = st.stake.clone() else {
                th::empty_state(
                    ui,
                    None,
                    "Load a wallet",
                    "load a wallet to see the local weight estimate",
                );
                return;
            };
            egui::Grid::new("stake_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Spendable:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {} across {} UTXOs",
                            s::format_tri(
                                st.derived
                                    .as_ref()
                                    .map(|d| d.utxos.iter().map(|u| u.value_sats).sum::<i64>())
                                    .unwrap_or(0)
                            ),
                            s::TICKER,
                            st.derived.as_ref().map(|d| d.utxos.len()).unwrap_or(0)
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Stake (weight):").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} coin-days ({} mature coins, {} young <1h)",
                            live.field("weight"),
                            live.field("coins_meeting_min_age"),
                            young_n
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("PoS difficulty / target:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} / {}",
                            live.field("difficulty"),
                            live.field("target")
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Stake modifier (tip):").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{}{}",
                            live.field("modifier"),
                            if stake.modifier_generated { " (generated)" } else { "" }
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Kernel attempts:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} attempts / {} searches (last pass {}s)",
                            live.field("attempts"),
                            live.field("searches"),
                            live.field("last_search_secs")
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Last stake:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(
                            live.info
                                .as_ref()
                                .and_then(|v| v.get("last_stake"))
                                .map(|ls| {
                                    format!(
                                        "h{} {} at {}",
                                        ls.get("height").and_then(|x| x.as_u64()).unwrap_or(0),
                                        ls.get("hash").and_then(|x| x.as_str()).unwrap_or("?"),
                                        ls.get("at_utc").and_then(|x| x.as_str()).unwrap_or("?")
                                    )
                                })
                                .unwrap_or_else(|| "none this session".into()),
                        )
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Relay pool:").color(th::TEXT_SECONDARY));
                    ui.label(
                        egui::RichText::new(format!(
                            "{} pooled tx(s) (fees attach to the next stake)",
                            live.field("pooledtx")
                        ))
                        .monospace()
                        .color(th::TEXT_PRIMARY),
                    );
                    ui.end_row();
                    if let Some(reasons) = live
                        .info
                        .as_ref()
                        .and_then(|v| v.get("staking_disabled_reasons"))
                        .and_then(|r| r.as_array())
                        .filter(|r| !r.is_empty())
                    {
                        ui.label(egui::RichText::new("Disabled because:").color(th::WARNING));
                        ui.label(
                            reasons
                                .iter()
                                .filter_map(|r| r.as_str())
                                .collect::<Vec<_>>()
                                .join("; "),
                        );
                        ui.end_row();
                    }
                });
            if !stake.note.is_empty() {
                ui.add_space(4.0);
                th::text_meta(ui, &stake.note);
            }
            // raw RPC disclosure for diagnostics
            egui::CollapsingHeader::new("Raw getstakinginfo")
                .default_open(false)
                .show(ui, |ui| {
                    let raw = live
                        .info
                        .as_ref()
                        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                        .unwrap_or_else(|| "—".into());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(raw).monospace().small().color(th::TEXT_SECONDARY),
                        )
                        .wrap(),
                    );
                });
            th::text_meta(ui, "Reserve editing lives in Options.");
            if th::ghost_button(ui, "Open Options").clicked() {
                open_options = true;
            }
        });
        if open_options {
            if self.st.screen != Screen::Options {
                self.options_return = Some(Screen::Staking);
            }
            self.st.goto(Screen::Options);
        }
    }

    // -----------------------------------------------------------------------
    // §10.7 Messages — master/detail (Inbox / Sent / Compose)
    // -----------------------------------------------------------------------
    fn msg_segmented(&mut self, ui: &mut egui::Ui) {
        let unread = self.st.messages.unread_count();
        ui.horizontal(|ui| {
            let tabs = [
                (MsgTab::Inbox, format!("Inbox{}", if unread > 0 { format!(" ({})", unread) } else { String::new() })),
                (MsgTab::Sent, "Sent".to_string()),
                (MsgTab::Compose, "Compose".to_string()),
            ];
            for (tab, label) in tabs {
                let selected = self.msg_tab == tab;
                let ir = th::table_row(ui, 0, selected, 120.0, |rui| {
                    th::row_click(rui, 30.0);
                    th::table_cell_text(
                        rui,
                        0.0,
                        120.0,
                        &label,
                        false,
                        if selected { th::TEXT_PRIMARY } else { th::TEXT_SECONDARY },
                        false,
                    );
                    true
                });
                if ir.inner {
                    if self.msg_tab != tab {
                        self.msg_selected = None;
                    }
                    self.msg_tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if th::secondary_button(ui, "Refresh").clicked() {
                    self.st.messages_refresh_action();
                }
                if th::ghost_button(ui, "Purge expired").clicked() {
                    self.st.messages_purge_action();
                }
            });
        });
    }

    fn page_messages(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Messages",
            "Encrypted messages sent and received through Triangles",
        );
        self.msg_segmented(ui);
        ui.add_space(8.0);
        if let Some(e) = &self.st.messages.last_error {
            th::inline_notice(ui, th::Notice::Error, "RPC error", e);
            ui.add_space(4.0);
        }
        match self.msg_tab {
            MsgTab::Inbox => self.messages_inbox(ui),
            MsgTab::Sent => self.messages_sent(ui),
            MsgTab::Compose => self.messages_compose(ui),
        }
        // message diagnostics disclosure (raw RPC/local transport data)
        egui::CollapsingHeader::new("Message diagnostics")
            .default_open(false)
            .show(ui, |ui| {
                let li = self.st.messages.localinfo.clone();
                if let Some(li) = &li {
                    th::text_meta(
                        ui,
                        &format!(
                            "keys {} · buckets {} · received {} · sent {}",
                            li.get("keys").and_then(|x| x.as_u64()).unwrap_or(0),
                            li.get("buckets").and_then(|x| x.as_u64()).unwrap_or(0),
                            li.get("received").and_then(|x| x.as_u64()).unwrap_or(0),
                            li.get("sent").and_then(|x| x.as_u64()).unwrap_or(0),
                        ),
                    );
                } else {
                    th::text_meta(ui, "press Refresh to load (tri7d must run with --wallet)");
                }
            });
    }

    fn messages_inbox(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        match st.messages.inbox.clone() {
            Some(Value::Array(rows)) if !rows.is_empty() => {
                let sel = self.msg_selected;
                th::card(ui, |ui| {
                    let w = ui.available_width();
                    let preview_w = (w - 24.0 - 260.0 - 130.0).max(140.0);
                    let cols = [
                        th::TableCol::new("", 24.0, false),
                        th::TableCol::new("Contact", 260.0, false),
                        th::TableCol::new("Preview", preview_w, false),
                        th::TableCol::new("Date", 130.0, true),
                    ];
                    th::table_header(ui, &cols, w);
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for (i, r) in rows.iter().enumerate() {
                                let unread = r.get("read") == Some(&Value::Bool(false));
                                let from = r
                                    .get("from")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                let text = r
                                    .get("text")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let preview = truncate_chars(&text, 48);
                                let date = fmt_unix(
                                    r.get("time").and_then(|x| x.as_i64()).unwrap_or(0) as u32,
                                );
                                let selected = sel == Some(i);
                                let ir = th::table_row(ui, i, selected, w, |rui| {
                                    let resp = th::row_click(rui, 38.0);
                                    if unread {
                                        let rect = egui::Rect::from_min_size(
                                            egui::pos2(
                                                rui.max_rect().left() + 10.0,
                                                rui.max_rect().top() + 15.0,
                                            ),
                                            Vec2::splat(8.0),
                                        );
                                        rui.painter()
                                            .circle_filled(rect.center(), 4.0, th::BRAND_RED);
                                    }
                                    th::table_cell_text(rui, 24.0, cols[1].width, &from, false, if unread { th::TEXT_PRIMARY } else { th::TEXT_SECONDARY }, true);
                                    th::table_cell_text(rui, 284.0, cols[2].width, &preview, false, th::TEXT_PRIMARY, false);
                                    th::table_cell_text(
                                        rui,
                                        284.0 + preview_w,
                                        cols[3].width,
                                        &date,
                                        true,
                                        th::TEXT_MUTED,
                                        false,
                                    );
                                    resp.clicked()
                                });
                                if ir.inner {
                                    self.msg_selected = Some(i);
                                }
                            }
                        });
                });
                // reading pane
                if let Some(i) = sel {
                    if let Some(r) = rows.get(i) {
                        self.messages_reading_pane(
                            ui,
                            r.get("from").and_then(|x| x.as_str()).unwrap_or("?"),
                            r.get("to").and_then(|x| x.as_str()).unwrap_or(""),
                            r.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                            &format!(
                                "t={} b={}",
                                r.get("time").and_then(|x| x.as_i64()).unwrap_or(0),
                                r.get("bytes").and_then(|x| x.as_u64()).unwrap_or(0)
                            ),
                        );
                    }
                }
            }
            Some(Value::Array(_)) => {
                th::card(ui, |ui| {
                    th::empty_state(ui, None, "Inbox empty", "No encrypted messages yet");
                });
            }
            _ => {
                th::card(ui, |ui| {
                    th::empty_state(
                        ui,
                        None,
                        "Inbox not loaded",
                        "press Refresh to load (tri7d must run with --wallet)",
                    );
                });
            }
        }
    }

    fn messages_sent(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        match st.messages.outbox.clone() {
            Some(Value::Array(rows)) if !rows.is_empty() => {
                let sel = self.msg_selected;
                th::card(ui, |ui| {
                    let w = ui.available_width();
                    let preview_w = (w - 24.0 - 260.0 - 130.0).max(140.0);
                    let cols = [
                        th::TableCol::new("", 24.0, false),
                        th::TableCol::new("To", 260.0, false),
                        th::TableCol::new("Preview", preview_w, false),
                        th::TableCol::new("Date", 130.0, true),
                    ];
                    th::table_header(ui, &cols, w);
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            for (i, r) in rows.iter().enumerate() {
                                let to = r
                                    .get("to")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                let text = r
                                    .get("text")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let preview = truncate_chars(&text, 48);
                                let date = fmt_unix(
                                    r.get("time").and_then(|x| x.as_i64()).unwrap_or(0) as u32,
                                );
                                let selected = sel == Some(i);
                                let ir = th::table_row(ui, i, selected, w, |rui| {
                                    let resp = th::row_click(rui, 38.0);
                                    th::table_cell_text(rui, 24.0, cols[1].width, &to, false, th::TEXT_PRIMARY, true);
                                    th::table_cell_text(rui, 284.0, cols[2].width, &preview, false, th::TEXT_PRIMARY, false);
                                    th::table_cell_text(
                                        rui,
                                        284.0 + preview_w,
                                        cols[3].width,
                                        &date,
                                        true,
                                        th::TEXT_MUTED,
                                        false,
                                    );
                                    resp.clicked()
                                });
                                if ir.inner {
                                    self.msg_selected = Some(i);
                                }
                            }
                        });
                });
                if let Some(i) = sel {
                    if let Some(r) = rows.get(i) {
                        self.messages_reading_pane(
                            ui,
                            r.get("from").and_then(|x| x.as_str()).unwrap_or(""),
                            r.get("to").and_then(|x| x.as_str()).unwrap_or("?"),
                            r.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                            &format!("t={}", r.get("time").and_then(|x| x.as_i64()).unwrap_or(0)),
                        );
                    }
                }
            }
            _ => {
                th::card(ui, |ui| {
                    th::empty_state(ui, None, "Outbox empty", "Sent messages will appear here");
                });
            }
        }
    }

    /// Reading pane: From/To/date + full body + reply/copy actions.
    fn messages_reading_pane(
        &mut self,
        ui: &mut egui::Ui,
        from: &str,
        to: &str,
        body: &str,
        meta: &str,
    ) {
        ui.add_space(8.0);
        th::card_titled(ui, "Message", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("From:").color(th::TEXT_SECONDARY));
                ui.label(egui::RichText::new(from).monospace().color(th::TEXT_PRIMARY));
                ui.label(egui::RichText::new("To:").color(th::TEXT_SECONDARY));
                ui.label(egui::RichText::new(to).monospace().color(th::TEXT_PRIMARY));
                ui.label(egui::RichText::new(meta).small().color(th::TEXT_MUTED));
            });
            ui.add_space(4.0);
            egui::Frame::none()
                .fill(th::INPUT_BG)
                .stroke(Stroke::new(1.0, th::BORDER_SUBTLE))
                .rounding(Rounding::same(4.0))
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(body).color(th::TEXT_PRIMARY),
                        )
                        .wrap(),
                    );
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !from.is_empty() && th::secondary_button(ui, "Reply").clicked() {
                    self.st.messages.compose_to = from.to_string();
                    self.msg_tab = MsgTab::Compose;
                    self.msg_selected = None;
                }
                if !from.is_empty() && th::ghost_button(ui, "Copy From Address").clicked() {
                    ui.output_mut(|o| o.copied_text = from.to_string());
                }
                if !to.is_empty() && th::ghost_button(ui, "Copy To Address").clicked() {
                    ui.output_mut(|o| o.copied_text = to.to_string());
                }
                if th::ghost_button(ui, "Back").clicked() {
                    self.msg_selected = None;
                }
            });
        });
    }

    fn messages_compose(&mut self, ui: &mut egui::Ui) {
        th::card_titled(ui, "Send an encrypted message", |ui| {
            let st = &mut self.st;
            egui::Grid::new("smsg_compose")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("From:").color(th::TEXT_SECONDARY));
                    let from = &mut st.messages.compose_from;
                    let sel = if from.is_empty() {
                        "select a wallet key".to_string()
                    } else {
                        from.clone()
                    };
                    egui::ComboBox::from_id_source("smsg_from")
                        .selected_text(sel)
                        .width(420.0)
                        .show_ui(ui, |ui| {
                            if let Some(li) = &st.messages.localinfo {
                                if let Some(addrs) = li.get("addresses").and_then(|a| a.as_array())
                                {
                                    for a in addrs {
                                        if let Some(addr) = a.as_str() {
                                            ui.selectable_value(from, addr.to_string(), addr);
                                        }
                                    }
                                }
                            }
                        });
                    ui.end_row();
                    ui.label(egui::RichText::new("To:").color(th::TEXT_SECONDARY));
                    ui.horizontal(|ui| {
                        th::text_input(ui, 420.0, &mut st.messages.compose_to);
                        let book: Vec<String> = st
                            .all_book_rows()
                            .iter()
                            .map(|(a, _, _)| a.clone())
                            .collect();
                        let book_resp = egui::ComboBox::from_id_source("smsg_to_book")
                            .selected_text("address book")
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for a in &book {
                                    let d = &mut st.messages.compose_to;
                                    if ui.selectable_label(d != a, a.clone()).clicked() {
                                        *d = a.clone();
                                    }
                                }
                            });
                        book_resp
                            .response
                            .on_hover_text("Pick an address from the address book");
                    });
                    ui.end_row();
                    ui.label(egui::RichText::new("Message:").color(th::TEXT_SECONDARY));
                    // multiline compose area (draft retained on failure)
                    th::multiline_area(ui, 480.0, 120.0, &mut st.messages.compose_text);
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if th::primary_button(ui, "Send").clicked() {
                    st.messages_send_action();
                }
                match &st.messages.send_result {
                    Some(Ok(msg)) => {
                        th::inline_notice(ui, th::Notice::Success, "Sent", msg);
                    }
                    Some(Err(e)) => {
                        // draft retained in state — inline error (spec §10.7)
                        th::inline_notice(ui, th::Notice::Error, "Send failed — draft kept", e);
                    }
                    None => {}
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // §10.10 Sign and Verify Message (tool pages)
    // -----------------------------------------------------------------------
    fn page_sign(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Sign Message",
            "Sign a message to prove you own a TRI address",
        );
        let addrs: Vec<String> = {
            let mut v: Vec<String> = self.st.key_map().keys().cloned().collect();
            v.sort();
            v
        };
        th::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Address:").color(th::TEXT_SECONDARY));
                let sel = if self.st.sign.address.is_empty() {
                    "select address".to_string()
                } else {
                    self.st.sign.address.clone()
                };
                egui::ComboBox::from_id_source("sign_addr")
                    .selected_text(sel)
                    .width(420.0)
                    .show_ui(ui, |ui| {
                        for a in &addrs {
                            let addr = &mut self.st.sign.address;
                            ui.selectable_value(addr, a.clone(), a.clone());
                        }
                    });
                // address-book picker
                let book: Vec<String> = self
                    .st
                    .all_book_rows()
                    .iter()
                    .map(|(a, _, _)| a.clone())
                    .collect();
                let book_resp = egui::ComboBox::from_id_source("sign_addr_book")
                    .selected_text("address book")
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for a in &book {
                            let d = &mut self.st.sign.address;
                            if ui.selectable_label(d != a, a.clone()).clicked() {
                                *d = a.clone();
                            }
                        }
                    });
                book_resp
                    .response
                    .on_hover_text("Pick an address from the address book");
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Message:").color(th::TEXT_SECONDARY));
            th::multiline_area(ui, 520.0, 96.0, &mut self.st.sign.message);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if th::primary_button(ui, "Sign message").clicked() {
                    self.st.sign_message_action();
                }
                if th::secondary_button(ui, "Clear").clicked() {
                    self.st.sign.message.clear();
                    self.st.sign.signature.clear();
                    self.st.sign.error = None;
                }
            });
            if let Some(e) = &self.st.sign.error {
                ui.add_space(4.0);
                th::inline_notice(ui, th::Notice::Error, "Sign failed", e);
            }
            if !self.st.sign.signature.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Signature:").color(th::TEXT_SECONDARY));
                // multiline monospace selectable signature panel
                egui::Frame::none()
                    .fill(th::INPUT_BG)
                    .stroke(Stroke::new(1.0, th::BORDER_SUBTLE))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&self.st.sign.signature)
                                    .monospace()
                                    .color(th::TEXT_PRIMARY),
                            )
                            .wrap(),
                        );
                    });
                if th::ghost_button(ui, "Copy signature").clicked() {
                    ui.output_mut(|o| o.copied_text = self.st.sign.signature.clone());
                }
            }
        });
    }

    fn page_verify(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Verify Message",
            "Verify a message to ensure it was signed with a specified TRI address",
        );
        th::card(ui, |ui| {
            egui::Grid::new("verify_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Address:").color(th::TEXT_SECONDARY));
                    th::text_input(ui, 420.0, &mut self.st.verify.address);
                    ui.end_row();
                    ui.label(egui::RichText::new("Signature:").color(th::TEXT_SECONDARY));
                    th::text_input(ui, 420.0, &mut self.st.verify.signature);
                    ui.end_row();
                    ui.label(egui::RichText::new("Message:").color(th::TEXT_SECONDARY));
                    th::multiline_area(ui, 420.0, 96.0, &mut self.st.verify.message);
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if th::primary_button(ui, "Verify message").clicked() {
                    self.st.verify_message_action();
                }
                if th::secondary_button(ui, "Clear").clicked() {
                    self.st.verify.address.clear();
                    self.st.verify.signature.clear();
                    self.st.verify.message.clear();
                    self.st.verify.result = None;
                }
            });
            // bordered semantic result panel (never only colored text)
            match &self.st.verify.result {
                Some(Ok(true)) => {
                    ui.add_space(4.0);
                    th::inline_notice(
                        ui,
                        th::Notice::Success,
                        "Message verified",
                        "the signature matches the address",
                    );
                }
                Some(Ok(false)) => {
                    ui.add_space(4.0);
                    th::inline_notice(
                        ui,
                        th::Notice::Error,
                        "Message verification FAILED",
                        "the signature does not match the address",
                    );
                }
                Some(Err(e)) => {
                    ui.add_space(4.0);
                    th::inline_notice(ui, th::Notice::Error, "Verification error", e);
                }
                None => {}
            }
        });
    }

    // -----------------------------------------------------------------------
    // §10.9 Options — historical tab structure with sticky footer
    // -----------------------------------------------------------------------
    fn options_tabs(&mut self, ui: &mut egui::Ui) {
        let tabs = ["Main", "Network", "Window", "Display"];
        ui.horizontal(|ui| {
            for (i, label) in tabs.iter().enumerate() {
                let selected = self.options_tab == i;
                let ir = th::table_row(ui, 0, selected, 130.0, |rui| {
                    th::row_click(rui, 30.0);
                    th::table_cell_text(
                        rui,
                        0.0,
                        130.0,
                        label,
                        false,
                        if selected { th::TEXT_PRIMARY } else { th::TEXT_SECONDARY },
                        false,
                    );
                    true
                });
                if ir.inner {
                    self.options_tab = i;
                }
            }
        });
        ui.add_space(8.0);
    }

    fn page_options(&mut self, ui: &mut egui::Ui) {
        th::page_header(ui, "Options", "Wallet and interface preferences");
        self.options_tabs(ui);
        th::card(ui, |ui| match self.options_tab {
            0 => {
                // Main
                egui::Grid::new("opt_main")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("Pay transaction &fee:")
                                .color(th::TEXT_SECONDARY),
                        );
                        ui.horizontal(|ui| {
                            th::text_input(ui, 140.0, &mut self.st.options.pay_fee);
                            ui.label(egui::RichText::new(s::TICKER).color(th::TEXT_MUTED));
                        });
                        ui.end_row();
                        ui.label(egui::RichText::new("Reserve:").color(th::TEXT_SECONDARY));
                        th::disabled_input(
                            ui,
                            140.0,
                            &self.st.options.reserve,
                            "Requires daemon integration (staking reserve)",
                        );
                        ui.end_row();
                        ui.label(
                            egui::RichText::new("Start Triangles on login:")
                                .color(th::TEXT_SECONDARY),
                        );
                        th::disabled_input(
                            ui,
                            140.0,
                            "—",
                            "Requires daemon integration",
                        );
                        ui.end_row();
                        ui.label(egui::RichText::new("Detach databases:").color(th::TEXT_SECONDARY));
                        th::disabled_input(ui, 140.0, "—", "Requires daemon integration");
                        ui.end_row();
                    });
                th::text_meta(ui, "Staking reserve belongs here (daemon integration pending).");
                ui.add_space(8.0);
                ui.separator();
                // ---- Software updates (src/updater.rs) ----
                ui.label(
                    egui::RichText::new("Software updates")
                        .strong()
                        .color(th::TEXT_PRIMARY),
                );
                ui.label(
                    egui::RichText::new(self.st.updater_options_line())
                        .size(12.0)
                        .color(th::TEXT_SECONDARY),
                );
                ui.label(
                    egui::RichText::new(
                        "Checked on startup and every 6 hours from GitHub Releases; \
                         nothing downloads or installs without your confirmation.",
                    )
                    .size(12.0)
                    .color(th::TEXT_MUTED),
                );
                ui.horizontal(|ui| {
                    if th::secondary_button(ui, "Check for updates").clicked() {
                        self.st.updater_manual_check_action();
                    }
                    if th::ghost_button(ui, "Restore previous version")
                        .clicked()
                    {
                        match self.st.updater_restore_previous_action() {
                            Ok(desc) => self.st.status_log.push(format!("update: {desc}")),
                            Err(e) => self.st.status_log.push(format!("update: {e}")),
                        }
                    }
                });
                if th::ghost_button(ui, "View latest release notes").clicked() {
                    self.st.updater_view_notes_action();
                }
                if matches!(
                    self.st.updater.phase,
                    crate::updater::UpdatePhase::Available
                ) {
                    if th::primary_button(ui, "Download & Install").clicked() {
                        if let Err(e) = self.st.updater_download_action() {
                            self.st.status_log.push(format!("update: {e}"));
                        }
                    }
                }
                if let crate::updater::UpdatePhase::Failed(err) = &self.st.updater.phase {
                    th::inline_notice(ui, th::Notice::Error, "Update failed", err);
                }
            }
            1 => {
                // Network
                let mut off = false;
                ui.add_enabled(false, egui::Checkbox::new(&mut off, "Map port using UPnP"))
                    .on_disabled_hover_text("Requires daemon integration");
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Proxy:").color(th::TEXT_SECONDARY));
                    th::disabled_input(ui, 140.0, "—", "Not supported by this desktop shell");
                    th::disabled_input(ui, 90.0, "—", "Not supported by this desktop shell");
                    th::disabled_input(ui, 90.0, "—", "Not supported by this desktop shell");
                    th::disabled_input(ui, 90.0, "—", "Not supported by this desktop shell");
                });
                th::text_meta(ui, "Proxy enable/IP/port/version require daemon integration.");
            }
            2 => {
                // Window
                let mut off = false;
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut off, "Minimize to the tray instead of the taskbar"),
                )
                .on_disabled_hover_text("Not supported by this desktop shell");
                let mut off2 = false;
                ui.add_enabled(false, egui::Checkbox::new(&mut off2, "Minimize on close"))
                    .on_disabled_hover_text("Not supported by this desktop shell");
            }
            _ => {
                // Display
                ui.checkbox(
                    &mut self.st.options.display_addresses,
                    "Display addresses in transaction list",
                );
                ui.checkbox(
                    &mut self.st.options.coin_control,
                    "Display coin control features (experts only!)",
                );
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Language:").color(th::TEXT_SECONDARY));
                    th::disabled_input(ui, 160.0, "English", "Not supported by this desktop shell");
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Amount unit:").color(th::TEXT_SECONDARY));
                    th::disabled_input(ui, 160.0, s::TICKER, "Not supported by this desktop shell");
                });
                let mut off = false;
                ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut off, "Show onion address in status bar"),
                )
                .on_disabled_hover_text("Requires daemon integration");
            }
        });
        // sticky footer: Cancel / Apply / OK
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            let ret = self.options_return.unwrap_or(Screen::Overview);
            if th::ghost_button(ui, "Cancel").clicked() {
                self.st.goto(ret);
            }
            if th::secondary_button(ui, "Apply").clicked() {
                match self.st.save_options_action() {
                    Ok(()) => self.st.status_log.push("options saved".into()),
                    Err(e) => self.st.status_log.push(format!("options: {}", e)),
                }
            }
            if th::primary_button(ui, "OK").clicked() {
                let _ = self.st.save_options_action();
                self.st.goto(ret);
            }
            for line in self.st.status_log.iter().rev().take(2) {
                th::text_meta(ui, line);
            }
        });
    }

    // -----------------------------------------------------------------------
    // §10.8 Block Explorer — same data paths, shared cards/tables + tabs
    // -----------------------------------------------------------------------
    fn ex_tabs(&mut self, ui: &mut egui::Ui) {
        let tabs = [
            (ExTab::Overview, "Overview"),
            (ExTab::Block, "Block"),
            (ExTab::Transaction, "Transaction"),
            (ExTab::Address, "Address"),
            (ExTab::Rich, "Rich List"),
        ];
        ui.horizontal(|ui| {
            for (tab, label) in tabs {
                let selected = self.ex_tab == tab;
                let ir = th::table_row(ui, 0, selected, 120.0, |rui| {
                    th::row_click(rui, 30.0);
                    th::table_cell_text(
                        rui,
                        0.0,
                        120.0,
                        label,
                        false,
                        if selected { th::TEXT_PRIMARY } else { th::TEXT_SECONDARY },
                        false,
                    );
                    true
                });
                if ir.inner {
                    self.ex_tab = tab;
                }
            }
        });
        ui.add_space(8.0);
    }

    fn page_explorer(&mut self, ui: &mut egui::Ui) {
        th::page_header(
            ui,
            "Block Explorer",
            "Inspect blocks, transactions, addresses, and chain statistics",
        );
        // search field (height / block hash / txid / address)
        th::card(ui, |ui| {
            ui.horizontal(|ui| {
                let resp = th::text_input_h(
                    ui,
                    520.0,
                    &mut self.st.explorer.input,
                    Some("address / txid / block height / block hash"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.st.explorer_search_action();
                }
                if th::primary_button(ui, "Search").clicked() {
                    self.st.explorer_search_action();
                }
            });
            if let Some(e) = &self.st.explorer.last_error {
                ui.add_space(4.0);
                // errors appear immediately under search (spec §10.8)
                th::inline_notice(ui, th::Notice::Error, "Explorer error", e);
            }
        });
        self.ex_tabs(ui);
        // tabs preserve the existing explorer views; Overview is the default
        let active_view = match self.st.explorer.view.clone() {
            ex::ExplorerView::Landing => None,
            v => Some(v),
        };
        match self.ex_tab {
            ExTab::Overview => self.explorer_stats_view(ui),
            ExTab::Block => {
                if active_view == Some(ex::ExplorerView::Block) {
                    self.explorer_block_view(ui);
                } else {
                    th::card(ui, |ui| {
                        th::empty_state(
                            ui,
                            None,
                            "No block open",
                            "Search a block height or hash above",
                        );
                    });
                }
            }
            ExTab::Transaction => {
                if active_view == Some(ex::ExplorerView::Tx) {
                    self.explorer_tx_view(ui);
                } else {
                    th::card(ui, |ui| {
                        th::empty_state(
                            ui,
                            None,
                            "No transaction open",
                            "Search a txid above",
                        );
                    });
                }
            }
            ExTab::Address => {
                if active_view == Some(ex::ExplorerView::Address) {
                    self.explorer_address_view(ui);
                } else {
                    th::card(ui, |ui| {
                        th::empty_state(
                            ui,
                            None,
                            "No address open",
                            "Search an address above",
                        );
                    });
                }
            }
            ExTab::Rich => self.explorer_rich_view(ui),
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
        th::card_titled(ui, &addr_str, |ui| {
            ui.horizontal(|ui| {
                if th::ghost_button(ui, "copy").clicked() {
                    ui.output_mut(|o| o.copied_text = addr_str.clone());
                }
            });
            egui::Grid::new("ex_addr_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Valid:").color(th::TEXT_SECONDARY));
                    ui.label(av.valid.to_string());
                    ui.end_row();
                    ui.label(egui::RichText::new("Kind:").color(th::TEXT_SECONDARY));
                    ui.label(av.kind.clone().unwrap_or_else(|| "n/a".into()));
                    ui.end_row();
                    ui.label(egui::RichText::new("In this wallet:").color(th::TEXT_SECONDARY));
                    ui.label(
                        av.ismine
                            .map(|b| b.to_string())
                            .unwrap_or_else(|| "n/a".into()),
                    );
                    ui.end_row();
                    ui.label(
                        egui::RichText::new("Received (regular txs, all-time):")
                            .color(th::TEXT_SECONDARY),
                    );
                    ui.label(
                        av.received_tri
                            .map(|r| {
                                format!(
                                    "{} {}",
                                    s::format_tri((r * 1_000_000.0).round() as i64),
                                    s::TICKER
                                )
                            })
                            .unwrap_or_else(|| "n/a".into()),
                    )
                    .on_hover_text("getreceivedbyaddress <addr> 0 — regular txs, P2PKH scripts only (legacy daemon semantics)");
                    ui.end_row();
                    ui.label(
                        egui::RichText::new("Received (snapshot):").color(th::TEXT_SECONDARY),
                    );
                    ui.label(
                        av.received_snapshot_sats
                            .map(|r| {
                                format!(
                                    "{} {} (h{} coverage)",
                                    s::format_tri(r),
                                    s::TICKER,
                                    st.explorer.rich.as_ref().map(|r| r.height).unwrap_or(2_201_090)
                                )
                            })
                            .unwrap_or_else(|| "n/a".into()),
                    );
                    ui.end_row();
                    ui.label(
                        egui::RichText::new("Balance (snapshot):").color(th::TEXT_SECONDARY),
                    );
                    ui.label(
                        av.balance_snapshot_sats
                            .map(|r| format!("{} {}", s::format_tri(r), s::TICKER))
                            .unwrap_or_else(|| "n/a".into()),
                    );
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
        th::card_titled(ui, "Recent activity (backward block scan)", |ui| {
            if st.explorer.scanning {
                ui.horizontal(|ui| {
                    ui.spinner();
                    th::text_meta(ui, "scanning blocks…");
                });
            }
            let w = ui.available_width();
            let txid_w = (w - 90.0 - 90.0 - 140.0).max(160.0);
            let cols = [
                th::TableCol::new("Height", 90.0, false),
                th::TableCol::new("Type", 90.0, false),
                th::TableCol::new("TxID", txid_w, false),
                th::TableCol::new("Amount", 140.0, true),
            ];
            th::table_header(ui, &cols, w);
            for (i, hit) in av.history.iter().enumerate() {
                let dir = if hit.received { "Received" } else { "Sent" };
                let txid_short = truncate_chars(&hit.txid, 24);
                let amt = format!(
                    "{}{} {}",
                    if hit.received { "+" } else { "−" },
                    s::format_tri(hit.sats),
                    s::TICKER
                );
                let h = hit.height.to_string();
                let color = if hit.received {
                    th::TX_IN_GREEN_BRIGHT
                } else {
                    th::ERROR
                };
                let txid_full = hit.txid.clone();
                let ir = th::table_row(ui, i, false, w, |rui| {
                    let resp = th::row_click(rui, 38.0);
                    th::table_cell_text(rui, 0.0, cols[0].width, &format!("h{}", h), false, th::TEXT_SECONDARY, true);
                    th::table_cell_text(rui, 90.0, cols[1].width, dir, false, th::TEXT_PRIMARY, false);
                    th::table_cell_text(rui, 180.0, cols[2].width, &txid_short, false, th::TEXT_PRIMARY, true);
                    th::table_cell_text(rui, 180.0 + txid_w, cols[3].width, &amt, true, color, true);
                    resp.clicked()
                });
                if ir.inner {
                    if st.explorer.fetch_tx(&txid_full).is_ok() {
                        st.explorer.open_tx = Some(txid_full.clone());
                        st.explorer.view = ex::ExplorerView::Tx;
                        self.ex_tab = ExTab::Transaction;
                    } else {
                        st.explorer.last_error = Some("tx fetch failed".into());
                    }
                }
            }
            th::text_meta(
                ui,
                &format!(
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
                ),
            );
            if !av.exhausted {
                let busy = st.explorer.scanning;
                let addr2 = addr_str.clone();
                if secondary_button_enabled(ui, !busy, "Scan 300 more blocks").clicked() {
                    st.explorer_attach_snapshot(&addr2);
                    st.explorer_scan_more_action(st.explorer.scan_budget_blocks);
                }
            }
        });
    }

    fn explorer_tx_view(&mut self, ui: &mut egui::Ui) {
        let st = &mut self.st;
        let Some(txid) = st.explorer.open_tx.clone() else {
            return;
        };
        let Some(tx) = st.explorer.txs.get(&txid).cloned() else {
            return;
        };
        th::card_titled(ui, "Transaction", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&txid).monospace().color(th::TEXT_PRIMARY),
                );
                if th::ghost_button(ui, "copy txid").clicked() {
                    ui.output_mut(|o| o.copied_text = txid.clone());
                }
            });
            egui::Grid::new("ex_tx_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Confirmations:").color(th::TEXT_SECONDARY));
                    ui.label(
                        tx.confirmations
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "n/a".into()),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Block height:").color(th::TEXT_SECONDARY));
                    ui.label(
                        tx.height
                            .map(|h| h.to_string())
                            .unwrap_or_else(|| "unconfirmed".into()),
                    );
                    ui.end_row();
                    ui.label(egui::RichText::new("Timestamp:").color(th::TEXT_SECONDARY));
                    ui.label(fmt_unix(tx.time));
                    ui.end_row();
                    ui.label(egui::RichText::new("Size:").color(th::TEXT_SECONDARY));
                    ui.label(format!("{} bytes", tx.size));
                    ui.end_row();
                });
            if let Some(bh) = &tx.blockhash {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Block:").color(th::TEXT_SECONDARY));
                    let short = truncate_chars(bh, 30);
                    if ui
                        .selectable_label(false, egui::RichText::new(short).monospace())
                        .clicked()
                    {
                        if let Ok(b) = st.explorer.fetch_block_by_hash(bh) {
                            st.explorer.open_block = Some(b.height);
                            st.explorer.view = ex::ExplorerView::Block;
                            self.ex_tab = ExTab::Block;
                        }
                    }
                });
            }
            // fee / stake credit
            let fee = st.fee_of(&txid);
            match fee {
                Some(f) if ex::tx_is_coinstake(&tx) => th::inline_notice(
                    ui,
                    th::Notice::Success,
                    "STAKE",
                    &format!("credit {} {}", s::format_tri(-f), s::TICKER),
                ),
                Some(f) => {
                    ui.label(
                        egui::RichText::new(format!("Fee: {} {}", s::format_tri(f), s::TICKER))
                            .monospace()
                            .color(th::TEXT_PRIMARY),
                    );
                }
                None => {
                    th::text_meta(ui, "Fee: n/a (coinbase / prevouts unresolved)");
                }
            }
        });
        ui.add_space(8.0);
        th::card_titled(ui, "Inputs", |ui| {
            let w = ui.available_width();
            let prev_w = (w - 240.0).max(160.0);
            let cols = [
                th::TableCol::new("Prevout", prev_w, false),
                th::TableCol::new("Kind", 240.0, false),
            ];
            th::table_header(ui, &cols, w);
            for (i, vin) in tx.vin.iter().enumerate() {
                match (&vin.prev_txid, vin.prev_n) {
                    (Some(pt), n) => {
                        let short = format!(
                            "{}:{}",
                            truncate_chars(pt, 24),
                            n.unwrap_or(0)
                        );
                        let pt2 = pt.clone();
                        let ir = th::table_row(ui, i, false, w, |rui| {
                            let resp = th::row_click(rui, 38.0);
                            th::table_cell_text(rui, 0.0, cols[0].width, &short, false, th::TEXT_PRIMARY, true);
                            th::table_cell_text(rui, prev_w, cols[1].width, "spent prevout", false, th::TEXT_SECONDARY, false);
                            resp.clicked()
                        });
                        if ir.inner {
                            if st.explorer.fetch_tx(&pt2).is_ok() {
                                st.explorer.open_tx = Some(pt2.clone());
                                st.explorer.view = ex::ExplorerView::Tx;
                            }
                        }
                    }
                    (None, _) => {
                        let cb = vin.coinbase.clone().unwrap_or_default();
                        th::table_row(ui, i, false, w, |rui| {
                            th::table_cell_text(rui, 0.0, cols[0].width, "coinbase", false, th::TEXT_SECONDARY, false);
                            th::table_cell_text(rui, prev_w, cols[1].width, &cb, false, th::TEXT_MUTED, true);
                        });
                    }
                }
            }
        });
        ui.add_space(8.0);
        th::card_titled(ui, "Outputs", |ui| {
            let w = ui.available_width();
            let addr_w = (w - 60.0 - 140.0 - 160.0).max(160.0);
            let cols = [
                th::TableCol::new("n", 60.0, false),
                th::TableCol::new("Value", 140.0, true),
                th::TableCol::new("Script", 160.0, false),
                th::TableCol::new("Address", addr_w, false),
            ];
            th::table_header(ui, &cols, w);
            for (i, vo) in tx.vout.iter().enumerate() {
                let n = vo.n.to_string();
                let val = format!("{} {}", s::format_tri(vo.value_sats), s::TICKER);
                let stype = vo.script_type.clone();
                let addr = ex::addr_of_vout(vo).unwrap_or_else(|| "—".into());
                let addr2 = addr.clone();
                let ir = th::table_row(ui, i, false, w, |rui| {
                    let resp = th::row_click(rui, 38.0).on_hover_text("open address");
                    th::table_cell_text(rui, 0.0, cols[0].width, &n, false, th::TEXT_SECONDARY, true);
                    th::table_cell_text(rui, 60.0, cols[1].width, &val, true, th::TEXT_PRIMARY, true);
                    th::table_cell_text(rui, 200.0, cols[2].width, &stype, false, th::TEXT_SECONDARY, false);
                    th::table_cell_text(rui, 360.0, cols[3].width, &addr, false, th::TEXT_PRIMARY, true);
                    resp.clicked()
                });
                if ir.inner && addr2 != "—" {
                    if st.explorer.open_address_action(&addr2).is_ok() {
                        st.explorer_attach_snapshot(&addr2);
                        if st.explorer.scan_address_more(&addr2, st.explorer.scan_budget_blocks).is_ok() {
                            st.explorer.view = ex::ExplorerView::Address;
                            self.ex_tab = ExTab::Address;
                        }
                    }
                }
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
        th::card_titled(ui, &format!("Block {}", b.height), |ui| {
            ui.horizontal(|ui| {
                if let Some(prev) = &b.prev {
                    if th::secondary_button(ui, "← prev").clicked() {
                        if let Some(&h) = st.explorer.block_height_of_hash.get(prev) {
                            if st.explorer.fetch_block_by_height(h).is_ok() {
                                st.explorer.open_block = Some(h);
                                st.explorer.view = ex::ExplorerView::Block;
                            }
                        }
                    }
                }
                if let Some(next) = &b.next {
                    if th::secondary_button(ui, "next →").clicked() {
                        if let Some(&h) = st.explorer.block_height_of_hash.get(next) {
                            if st.explorer.fetch_block_by_height(h).is_ok() {
                                st.explorer.open_block = Some(h);
                                st.explorer.view = ex::ExplorerView::Block;
                            }
                        }
                    }
                }
                // PoS badge: BRAND_RED outlined; PoW neutral (spec §10.8)
                if b.pos {
                    ui.label(
                        egui::RichText::new("PoS")
                            .size(11.0)
                            .strong()
                            .color(th::BRAND_RED),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("PoW").size(11.0).strong().color(th::TEXT_MUTED),
                    );
                }
            });
            egui::Grid::new("ex_blk_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Hash:").color(th::TEXT_SECONDARY));
                    ui.label(egui::RichText::new(&b.hash).monospace().color(th::TEXT_PRIMARY));
                    ui.end_row();
                    ui.label(egui::RichText::new("Timestamp:").color(th::TEXT_SECONDARY));
                    ui.label(fmt_unix(b.time));
                    ui.end_row();
                    ui.label(egui::RichText::new("nBits:").color(th::TEXT_SECONDARY));
                    ui.label(&b.bits);
                    ui.end_row();
                    ui.label(egui::RichText::new("Difficulty:").color(th::TEXT_SECONDARY));
                    ui.label(b.difficulty.to_string());
                    ui.end_row();
                    ui.label(egui::RichText::new("Type:").color(th::TEXT_SECONDARY));
                    ui.label(if b.pos { "proof-of-stake" } else { "proof-of-work" });
                    ui.end_row();
                    ui.label(egui::RichText::new("Stake modifier:").color(th::TEXT_SECONDARY));
                    ui.label(format!(
                        "{}{}",
                        b.modifier.clone().unwrap_or_else(|| "n/a".into()),
                        if b.stake_modifier_generated { " (generated)" } else { "" }
                    ));
                    ui.end_row();
                    ui.label(egui::RichText::new("Modifier checksum:").color(th::TEXT_SECONDARY));
                    ui.label(b.modifierchecksum.clone().unwrap_or_else(|| "n/a".into()));
                    ui.end_row();
                    ui.label(egui::RichText::new("Entropy bit:").color(th::TEXT_SECONDARY));
                    ui.label(b.entropybit.map(|x| x.to_string()).unwrap_or_else(|| "n/a".into()));
                    ui.end_row();
                    ui.label(egui::RichText::new("Size / Version / Nonce:").color(th::TEXT_SECONDARY));
                    ui.label(format!("{} B / v{} / {}", b.size, b.version, b.nonce));
                    ui.end_row();
                    ui.label(egui::RichText::new("Confirmations:").color(th::TEXT_SECONDARY));
                    ui.label(
                        b.confirmations
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "n/a".into()),
                    );
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
        th::card_titled(ui, &format!("Transactions ({})", b.txids.len()), |ui| {
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for (i, txid) in b.txids.iter().enumerate() {
                        let short = truncate_chars(txid, 30);
                        let txid2 = txid.clone();
                        let ir = th::table_row(ui, i, false, 480.0, |rui| {
                            let resp = th::row_click(rui, 26.0);
                            th::table_cell_text(rui, 0.0, 480.0, &short, false, th::TEXT_PRIMARY, true);
                            resp.clicked()
                        });
                        if ir.inner {
                            if st.explorer.fetch_tx(&txid2).is_ok() {
                                st.explorer.open_tx = Some(txid2.clone());
                                st.explorer.view = ex::ExplorerView::Tx;
                                self.ex_tab = ExTab::Transaction;
                            } else {
                                st.explorer.last_error = Some("tx fetch failed".into());
                            }
                        }
                    }
                });
        });
    }

    fn explorer_stats_view(&mut self, ui: &mut egui::Ui) {
        let stats = self.st.explorer.stats.clone();
        th::card_titled(ui, "Chain statistics", |ui| {
            ui.horizontal(|ui| {
                if th::secondary_button(ui, "Refresh stats").clicked() {
                    self.st.explorer_stats_action(20);
                }
                th::text_meta(
                    ui,
                    &format!(
                        "endpoint: {}",
                        self.st
                            .explorer
                            .endpoint
                            .and_then(|i| self.st.explorer.endpoints.get(i))
                            .map(|sx| sx.clone())
                            .unwrap_or_else(|| "not probed".into())
                    ),
                );
            });
            ui.add_space(4.0);
            let Some(stats) = stats else {
                th::empty_state(
                    ui,
                    None,
                    "No chain statistics yet",
                    "Press Refresh stats to load them from tri7d",
                );
                return;
            };
            let g = |k: &str| stats.get(k).cloned().unwrap_or(Value::Null);
            egui::Grid::new("ex_stats")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Client:").color(th::TEXT_SECONDARY));
                    ui.label(g("subversion").as_str().unwrap_or("n/a").to_string());
                    ui.end_row();
                    ui.label(egui::RichText::new("Money supply:").color(th::TEXT_SECONDARY));
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
                    ui.label(egui::RichText::new("Tip height:").color(th::TEXT_SECONDARY));
                    ui.label(g("blocks").to_string());
                    ui.end_row();
                    ui.label(egui::RichText::new("Difficulty (PoS / PoW):").color(th::TEXT_SECONDARY));
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
                    ui.label(egui::RichText::new("Connections:").color(th::TEXT_SECONDARY));
                    ui.label(g("connections").to_string());
                    ui.end_row();
                    ui.label(egui::RichText::new("Testnet:").color(th::TEXT_SECONDARY));
                    ui.label(g("testnet").to_string());
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
        th::card_titled(ui, "Latest blocks", |ui| {
            let w = ui.available_width();
            let hash_w = (w - 90.0 - 60.0 - 150.0 - 170.0).max(160.0);
            let cols = [
                th::TableCol::new("Height", 90.0, false),
                th::TableCol::new("Type", 60.0, false),
                th::TableCol::new("Hash", hash_w, false),
                th::TableCol::new("Time", 150.0, false),
                th::TableCol::new("Kind", 170.0, false),
            ];
            th::table_header(ui, &cols, w);
            if self.st.explorer.latest.is_empty() {
                th::empty_state(ui, None, "No blocks listed", "Refresh stats to load recent blocks");
            }
            let latest = self.st.explorer.latest.clone();
            for (i, b) in latest.iter().enumerate() {
                let pos_label = if b.pos { "PoS" } else { "PoW" };
                let hash_short = truncate_chars(&b.hash, 24);
                let time = fmt_unix(b.time);
                let kind = if b.pos { "stake reward block" } else { "mined" };
                let height = b.height;
                let ir = th::table_row(ui, i, false, w, |rui| {
                    let resp = th::row_click(rui, 38.0);
                    th::table_cell_text(rui, 0.0, cols[0].width, &format!("h{}", height), false, th::TEXT_PRIMARY, true);
                    let badge_color = if b.pos { th::BRAND_RED } else { th::TEXT_MUTED };
                    th::table_cell_text(rui, 90.0, cols[1].width, pos_label, false, badge_color, false);
                    th::table_cell_text(rui, 150.0, cols[2].width, &hash_short, false, th::TEXT_PRIMARY, true);
                    th::table_cell_text(rui, 150.0 + hash_w, cols[3].width, &time, false, th::TEXT_SECONDARY, false);
                    th::table_cell_text(rui, 150.0 + hash_w + 150.0, cols[4].width, kind, false, th::TEXT_MUTED, false);
                    resp.clicked()
                });
                if ir.inner {
                    if self.st.explorer.fetch_block_by_height(height).is_ok() {
                        self.st.explorer.open_block = Some(height);
                        self.st.explorer.view = ex::ExplorerView::Block;
                        self.ex_tab = ExTab::Block;
                    }
                }
            }
        });
    }

    fn explorer_rich_view(&mut self, ui: &mut egui::Ui) {
        let rich = self.st.explorer.rich.clone();
        th::card_titled(ui, "Rich List", |ui| {
            ui.horizontal(|ui| {
                if th::secondary_button(ui, "Refresh rich list").clicked() {
                    self.st.explorer_rich_action(25);
                }
                if let Some(rl) = &rich {
                    th::text_meta(ui, &rl.source);
                }
            });
            let Some(rl) = rich else {
                th::empty_state(
                    ui,
                    None,
                    "No rich list yet",
                    "Press Refresh rich list to load it",
                );
                return;
            };
            let w = ui.available_width();
            let addr_w = (w - 60.0 - 200.0 - 140.0).max(180.0);
            let cols = [
                th::TableCol::new("#", 60.0, false),
                th::TableCol::new("Address", addr_w, false),
                th::TableCol::new("Balance", 200.0, true),
                th::TableCol::new("% of supply", 140.0, true),
            ];
            th::table_header(ui, &cols, w);
            for (i, (addr, sats)) in rl.entries.iter().enumerate() {
                let rank = format!("{}", i + 1);
                let bal = format!("{} {}", s::format_tri(*sats), s::TICKER);
                let pct = format!(
                    "{:.4}%",
                    if rl.total_chain_sats > 0 {
                        *sats as f64 / rl.total_chain_sats as f64 * 100.0
                    } else {
                        0.0
                    }
                );
                let addr2 = addr.clone();
                let ir = th::table_row(ui, i, false, w, |rui| {
                    let resp = th::row_click(rui, 38.0);
                    th::table_cell_text(rui, 0.0, cols[0].width, &rank, false, th::TEXT_SECONDARY, false);
                    th::table_cell_text(rui, 60.0, cols[1].width, &addr2, false, th::TEXT_PRIMARY, true);
                    th::table_cell_text(rui, 60.0 + addr_w, cols[2].width, &bal, true, th::TX_IN_GREEN_BRIGHT, true);
                    th::table_cell_text(rui, 60.0 + addr_w + 200.0, cols[3].width, &pct, true, th::TEXT_SECONDARY, true);
                    resp.clicked()
                });
                if ir.inner {
                    if self.st.explorer.open_address_action(&addr2).is_ok() {
                        self.st.explorer.view = ex::ExplorerView::Address;
                        self.ex_tab = ExTab::Address;
                    }
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Dialogs (About §10.11, Encrypt, enlarged QR preview)
    // -----------------------------------------------------------------------
    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.st.dialogs.about {
            let mut open = true;
            egui::Window::new("About Cryptographic Triangles")
                .open(&mut open)
                .collapsible(false)
                .default_width(480.0)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        // new canonical banner, centered, aspect-true
                        if let Some(tex) = self.tex(ui.ctx(), "brand/header") {
                            ui.add(img(&tex, Vec2::new(257.0, 54.0))); // 1199x252 aspect
                        }
                    });
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Cryptographic Triangles")
                            .size(17.0)
                            .strong()
                            .color(th::TEXT_PRIMARY),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "tri7 version {}",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .color(th::TEXT_SECONDARY),
                    );
                    ui.label(
                        egui::RichText::new("Modern client engine with byte-exact legacy-chain replay.")
                            .color(th::TEXT_SECONDARY),
                    );
                    egui::CollapsingHeader::new("Build details")
                        .default_open(false)
                        .show(ui, |ui| {
                            th::text_meta(
                                ui,
                                &format!(
                                    "tri7-core v{} (GUI) — rust secp256k1 (RFC6979, recovery), byte-exact legacy replay",
                                    env!("CARGO_PKG_VERSION")
                                ),
                            );
                        });
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if th::primary_button(ui, "OK").clicked() {
                            self.st.dialogs.about = false;
                        }
                    });
                });
            self.st.dialogs.about = open;
        }
        if let Some(msg) = self.st.dialogs.encrypt.clone() {
            let mut open = true;
            egui::Window::new("Encrypt Wallet")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    th::inline_notice(ui, th::Notice::Info, "Encrypt Wallet", &msg);
                    ui.add_space(4.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if th::primary_button(ui, "OK").clicked() {
                            self.st.dialogs.encrypt = None;
                        }
                    });
                });
            if !open {
                self.st.dialogs.encrypt = None;
            }
        }
        if self.st.dialogs.qr {
            // enlarged preview modal (the inline QR stays the primary view)
            let mut open = true;
            egui::Window::new("Request Payment")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    let qr_tex = self.qr_texture(ctx);
                    if let Some(tex) = &qr_tex {
                        egui::Frame::none()
                            .fill(Color32::WHITE)
                            .rounding(Rounding::same(4.0))
                            .inner_margin(egui::Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.add(img(tex, Vec2::splat(320.0)));
                            });
                    }
                    let uri = self.st.receive.uri.clone();
                    if !uri.is_empty() {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&uri)
                                    .monospace()
                                    .small()
                                    .color(th::TEXT_PRIMARY),
                            )
                            .wrap(),
                        );
                    }
                    ui.horizontal(|ui| {
                        if th::secondary_button(ui, "Copy URI").clicked() {
                            ui.output_mut(|o| o.copied_text = uri.clone());
                        }
                        th::text_input(ui, 220.0, &mut self.st.dialogs.qr_save_path);
                        if th::secondary_button(ui, "Save As...").clicked()
                            && !self.st.dialogs.qr_save_path.is_empty()
                        {
                            let p = self.st.dialogs.qr_save_path.clone();
                            match self.st.save_qr_action(&p) {
                                Ok(n) => self
                                    .st
                                    .status_log
                                    .push(format!("QR PNG saved to {} ({} bytes)", p, n)),
                                Err(e) => {
                                    self.st.status_log.push(format!("QR save failed: {}", e))
                                }
                            }
                        }
                    });
                });
            self.st.dialogs.qr = open;
        }
    }

    // -----------------------------------------------------------------------
    // Debug window (Settings menu) — full console functionality preserved
    // -----------------------------------------------------------------------
    fn debug_window(&mut self, ctx: &egui::Context) {
        if !self.console_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Triangles - Debug window")
            .open(&mut open)
            .collapsible(false)
            .default_width(680.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if th::secondary_button(ui, "Information (refresh)").clicked() {
                        self.st.refresh_debug_info_action();
                    }
                    th::text_section(ui, "Console");
                });
                ui.separator();
                let di = self.st.debug_info.clone().unwrap_or_default();
                egui::Grid::new("dbg_info")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Client name:").color(th::TEXT_SECONDARY));
                        ui.label("tri7-core");
                        ui.end_row();
                        ui.label(egui::RichText::new("Client version:").color(th::TEXT_SECONDARY));
                        ui.label(format!("v{} (phase 6 GUI)", env!("CARGO_PKG_VERSION")));
                        ui.end_row();
                        ui.label(egui::RichText::new("Crypto:").color(th::TEXT_SECONDARY));
                        ui.label("rust secp256k1 (RFC6979, recovery)");
                        ui.end_row();
                        ui.label(
                            egui::RichText::new("Number of connections:").color(th::TEXT_SECONDARY),
                        );
                        ui.label(di
                            .connections
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| di.error.clone().unwrap_or_else(|| "n/a".into())));
                        ui.end_row();
                        ui.label(
                            egui::RichText::new("Current number of blocks:").color(th::TEXT_SECONDARY),
                        );
                        ui.label(
                            self.st
                                .tip
                                .as_ref()
                                .map(|x| x.height.to_string())
                                .unwrap_or_else(|| "n/a".into()),
                        );
                        ui.end_row();
                        ui.label(egui::RichText::new("Last block time:").color(th::TEXT_SECONDARY));
                        ui.label(
                            self.st
                                .tip
                                .as_ref()
                                .map(|x| fmt_unix(x.tip_time))
                                .unwrap_or_else(|| "n/a".into()),
                        );
                        ui.end_row();
                        ui.label(egui::RichText::new("Debug log file:").color(th::TEXT_SECONDARY));
                        ui.label("tri7d journald unit (not a file on this host)");
                        ui.end_row();
                    });
                ui.separator();
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for e in self.st.console.iter().rev() {
                        ui.label(
                            egui::RichText::new(format!("> {}", e.request))
                                .monospace()
                                .color(th::TEXT_PRIMARY),
                        );
                        if let Some(r) = &e.reply {
                            th::text_meta(ui, r);
                        }
                        if let Some(err) = &e.error {
                            th::inline_notice(ui, th::Notice::Error, "RPC error", err);
                        }
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(">").color(th::TEXT_MUTED));
                    let resp = th::text_input(ui, 520.0, &mut self.st.console_input);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.st.rpc_console_send_action();
                    }
                    if th::primary_button(ui, "Send").clicked() {
                        self.st.rpc_console_send_action();
                    }
                    if th::ghost_button(ui, "Clear console").clicked() {
                        self.st.console.clear();
                    }
                });
                th::text_meta(
                    ui,
                    "read-only surface: gettip getblockcount getblockhash getblock getrawtransaction getinfo getdifficulty getconnectioncount getstakemodifier getbalance listunspent listwalletaddresses getreceivedbyaddress validateaddress health",
                );
            });
        self.console_open = open;
    }
}

// ---------------------------------------------------------------------------
// entry point (spec §5: preferred initial 1180×780, minimum 1040×700)
// ---------------------------------------------------------------------------

pub fn run(cfg: UiConfig) {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([1040.0, 700.0])
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
            poll_ticker: 0,
            send_step: SendStep::Details,
            msg_tab: MsgTab::Inbox,
            msg_selected: None,
            tx_selected: None,
            tx_search: String::new(),
            tx_min_amount: String::new(),
            tx_type_filter: 0,
            tx_status_filter: 0,
            ab_selected: None,
            ab_search: String::new(),
            ab_source_filter: 0,
            ab_edit: None,
            ab_delete: None,
            ab_new_label: String::new(),
            options_tab: 0,
            options_return: None,
            ex_tab: ExTab::Overview,
        }))
    });
    if let Err(e) = eframe::run_native("Cryptographic Triangles", opts, app) {
        eprintln!("eframe failed: {}", e);
        std::process::exit(1);
    }
}
