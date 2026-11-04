//! Production theme for the tri7 GUI — the REDESIGN_SPEC.md palette,
//! Visuals, spacing and shared view helpers.
//!
//! VIEW-LAYER ONLY: no engine/wallet logic lives here. Every constant mirrors
//! REDESIGN_SPEC.md §3 (exact production palette), §12.1 (Visuals), §12.2
//! (spacing) and §7 (shared components) verbatim.

use eframe::egui;
use egui::{
    pos2, Color32, Rounding, Sense, Stroke, TextureHandle, TextStyle, Ui, Vec2,
};

// ---------------------------------------------------------------------------
// §3 Exact production palette
// ---------------------------------------------------------------------------

pub const WINDOW_BG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00); // historical exact
pub const CONTENT_BG: Color32 = Color32::from_rgb(0x09, 0x09, 0x09); // modern derived
pub const PANEL_BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12); // modern bridge
pub const INPUT_BG: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x1C); // historical exact
pub const ROW_ALT: Color32 = Color32::from_rgb(0x17, 0x11, 0x0F); // palette-derived
pub const BORDER: Color32 = Color32::from_rgb(0x5B, 0x20, 0x13); // sampled red-brown
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(0x2D, 0x17, 0x12); // derived
pub const BRAND_RED: Color32 = Color32::from_rgb(0xE3, 0x21, 0x05); // historical exact
pub const BRAND_RED_HOVER: Color32 = Color32::from_rgb(0xF0, 0x3A, 0x18); // modern derived
pub const BRAND_RED_PRESSED: Color32 = Color32::from_rgb(0xC5, 0x1B, 0x04); // modern derived
pub const DEEP_RED: Color32 = Color32::from_rgb(0x3D, 0x0E, 0x04); // historical exact
pub const MENU_RED: Color32 = Color32::from_rgb(0xAF, 0x27, 0x2E); // historical exact
pub const MENU_SELECTED: Color32 = Color32::from_rgb(0x9D, 0x0B, 0x17); // historical exact
pub const COPPER: Color32 = Color32::from_rgb(0xA6, 0x54, 0x30); // sampled wide wordmark
pub const WARM_GOLD: Color32 = Color32::from_rgb(0xD3, 0x65, 0x32); // sampled wide wordmark
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF2, 0xE9, 0xE6); // modernized warm white
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xC8, 0xB7, 0xB2); // modernized warm gray
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8F, 0x77, 0x70); // modernized muted tier
pub const MENU_TEXT: Color32 = Color32::from_rgb(0xDE, 0xEE, 0xF1); // historical exact
pub const MENU_SELECTED_TEXT: Color32 = Color32::from_rgb(0xE9, 0xD9, 0xD8); // historical exact
pub const DISABLED_BG: Color32 = Color32::from_rgb(0x16, 0x16, 0x16); // modern replacement
pub const DISABLED_FG: Color32 = Color32::from_rgb(0x66, 0x53, 0x4E); // modern replacement
pub const SUCCESS: Color32 = Color32::from_rgb(0x55, 0xCC, 0x66);
/// Received-coins amount green (Sami directive): warm money-green, primary.
pub const TX_IN_GREEN: Color32 = Color32::from_rgb(0x3F, 0xA3, 0x4D);
/// Received-coins amount green — bright variant for text on black surfaces.
pub const TX_IN_GREEN_BRIGHT: Color32 = Color32::from_rgb(0x4C, 0xBB, 0x6C);
/// Received-coins green — darker variant for lighter surfaces.
pub const TX_IN_GREEN_DARK: Color32 = Color32::from_rgb(0x2E, 0x7D, 0x3B);
pub const WARNING: Color32 = Color32::from_rgb(0xE0, 0xA3, 0x3A);
pub const ERROR: Color32 = Color32::from_rgb(0xF0, 0x5A, 0x4A);
pub const INFO: Color32 = Color32::from_rgb(0x77, 0xAA, 0xDD);
/// Secondary-button pressed / global widget pressed surface (§7, §12.1).
pub const PRESSED_MAROON: Color32 = Color32::from_rgb(0x2A, 0x0B, 0x05);
/// Input hover border (§7 Inputs).
pub const INPUT_HOVER: Color32 = Color32::from_rgb(0x7A, 0x2B, 0x19);
/// Table header band (§7 Tables).
pub const TABLE_HEADER_BG: Color32 = Color32::from_rgb(0x14, 0x0B, 0x09);
/// Wordmark keyline color (§4.2) — recorded for provenance.
pub const WORDMARK_KEYLINE: Color32 = Color32::from_rgb(0x30, 0x08, 0x05);
/// Legacy disabled surface — provenance only; never use as a dark-theme bg.
pub const LEGACY_DISABLED_BG: Color32 = Color32::from_rgb(0xE0, 0xE0, 0xE0);
#[allow(dead_code)]
pub const HEADER_MAUVE: Color32 = Color32::from_rgb(0xAB, 0x7F, 0x82); // reference only

// ---------------------------------------------------------------------------
// §12.1 Theme constants and Visuals
// ---------------------------------------------------------------------------

/// Apply the production theme (visuals + spacing) to the context.
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;
    // Global default is warm-white body text; red stays for identity/action.
    v.override_text_color = Some(TEXT_PRIMARY);
    v.panel_fill = WINDOW_BG; // app shell, sidebar, header, status bar
    v.window_fill = PANEL_BG; // cards, dialogs (§12.1 window_fill #121212)
    v.extreme_bg_color = INPUT_BG; // inputs, combos, code areas
    v.faint_bg_color = PANEL_BG;
    v.code_bg_color = Color32::from_rgb(0x0D, 0x0D, 0x0D);
    v.selection.bg_fill = DEEP_RED;
    v.selection.stroke = Stroke::new(1.0, BRAND_RED);
    v.hyperlink_color = Color32::from_rgb(0xF0, 0x5A, 0x38);
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    v.window_rounding = Rounding::same(8.0);
    v.menu_rounding = Rounding::same(6.0);
    v.resize_corner_size = 6.0;

    // Widget visuals (§12.1).
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL_BG;
    w.noninteractive.weak_bg_fill = PANEL_BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    w.inactive.bg_fill = INPUT_BG;
    w.inactive.weak_bg_fill = INPUT_BG;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.hovered.bg_fill = DEEP_RED;
    w.hovered.weak_bg_fill = DEEP_RED;
    w.hovered.bg_stroke = Stroke::new(1.0, BRAND_RED);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.active.bg_fill = PRESSED_MAROON;
    w.active.weak_bg_fill = PRESSED_MAROON;
    w.active.bg_stroke = Stroke::new(1.0, BRAND_RED);
    w.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.open.bg_fill = DEEP_RED;
    w.open.weak_bg_fill = DEEP_RED;
    w.open.bg_stroke = Stroke::new(1.0, BRAND_RED);
    w.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    // Rounding: 4 px controls, 6 px cards/windows handled locally; expansion ≤ 1px.
    w.inactive.rounding = Rounding::same(4.0);
    w.hovered.rounding = Rounding::same(4.0);
    w.active.rounding = Rounding::same(4.0);
    w.noninteractive.rounding = Rounding::same(4.0);
    w.inactive.expansion = 1.0;
    w.hovered.expansion = 1.0;
    w.active.expansion = 1.0;
    w.open.expansion = 1.0;

    // §12.2 Spacing and sizing.
    let sp = &mut style.spacing;
    sp.item_spacing = Vec2::new(8.0, 8.0);
    sp.button_padding = Vec2::new(14.0, 8.0);
    sp.menu_margin = egui::Margin::same(6.0);
    sp.indent = 18.0;
    sp.interact_size = Vec2::new(36.0, 36.0);
    sp.slider_width = 160.0;
    sp.combo_width = 220.0;
    sp.text_edit_width = 280.0;

    // §4.1 explicit text styles (bundled font; family swap without layout change).
    style.text_styles = [
        (TextStyle::Heading, egui::FontId::proportional(25.0)), // page titles
        (TextStyle::Body, egui::FontId::proportional(14.0)),   // body copy
        (TextStyle::Button, egui::FontId::proportional(14.0)),
        (TextStyle::Small, egui::FontId::proportional(12.0)),  // small/meta
        (TextStyle::Monospace, egui::FontId::monospace(12.5)), // hashes, hex
    ]
    .into();

    ctx.set_style(style.clone());
    ctx.set_visuals(style.visuals.clone());
}

// ---------------------------------------------------------------------------
// §4.1 type roles
// ---------------------------------------------------------------------------

pub fn text_page_title(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(s).size(25.0).strong().color(TEXT_PRIMARY)),
    );
}

pub fn text_subtitle(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(s).size(13.0).color(TEXT_SECONDARY)),
    );
}

pub fn text_section(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(s).size(15.0).strong().color(TEXT_PRIMARY)),
    );
}

pub fn text_meta(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(s).size(12.0).color(TEXT_SECONDARY)),
    );
}

pub fn text_micro(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(s)
                .size(11.0)
                .strong()
                .color(TEXT_MUTED),
        ),
    );
}

// ---------------------------------------------------------------------------
// §7 Page header
// ---------------------------------------------------------------------------

/// Title + one-line purpose + 24 px red rule (2 px high) + 20 px gap.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    text_page_title(ui, title);
    text_subtitle(ui, subtitle);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(24.0, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, 1.0, BRAND_RED);
    ui.add_space(20.0);
}

// ---------------------------------------------------------------------------
// §7 Cards
// ---------------------------------------------------------------------------

/// Shared card: PANEL_BG, 1 px BORDER_SUBTLE, radius 6, 16 px padding.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::none()
        .fill(PANEL_BG)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, add_contents)
}

/// Card with a section title at top.
pub fn card_titled<R>(
    ui: &mut Ui,
    title: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    card(ui, |ui| {
        text_section(ui, title);
        ui.add_space(6.0);
        add_contents(ui)
    })
}

/// Hover-lifting card (border brightens to BORDER; no shadows).
pub fn card_hover<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let lift = ui
        .input(|i| i.pointer.hover_pos())
        .map(|p| ui.max_rect().contains(p))
        .unwrap_or(false);
    egui::Frame::none()
        .fill(PANEL_BG)
        .stroke(Stroke::new(1.0, if lift { BORDER } else { BORDER_SUBTLE }))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, add_contents)
}

// ---------------------------------------------------------------------------
// §7 Buttons — local constructors (global widget visuals cannot express all
// four variants; spec §12.1).
// ---------------------------------------------------------------------------

fn button_core(ui: &mut Ui, label: &str, height: f32) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label))
            .min_size(Vec2::new(0.0, height)),
    )
}

/// Primary: BRAND_RED / hover #F03A18 / pressed #C51B04, black semibold text.
pub fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = BRAND_RED;
        v.inactive.bg_fill = BRAND_RED;
        v.inactive.bg_stroke = Stroke::NONE;
        v.inactive.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.hovered.weak_bg_fill = BRAND_RED_HOVER;
        v.hovered.bg_fill = BRAND_RED_HOVER;
        v.hovered.bg_stroke = Stroke::NONE;
        v.hovered.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.active.weak_bg_fill = BRAND_RED_PRESSED;
        v.active.bg_fill = BRAND_RED_PRESSED;
        v.active.bg_stroke = Stroke::NONE;
        v.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        button_core(ui, label, 36.0)
    })
    .inner
}

/// Large transaction action (42 px) — Send page primary footer.
pub fn primary_button_large(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = BRAND_RED;
        v.inactive.bg_fill = BRAND_RED;
        v.inactive.bg_stroke = Stroke::NONE;
        v.inactive.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.hovered.weak_bg_fill = BRAND_RED_HOVER;
        v.hovered.bg_fill = BRAND_RED_HOVER;
        v.hovered.bg_stroke = Stroke::NONE;
        v.hovered.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        v.active.weak_bg_fill = BRAND_RED_PRESSED;
        v.active.bg_fill = BRAND_RED_PRESSED;
        v.active.bg_stroke = Stroke::NONE;
        v.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        button_core(ui, label, 42.0)
    })
    .inner
}

/// Secondary: INPUT_BG + 1 px BORDER / hover DEEP_RED + BRAND_RED / #2A0B05.
pub fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = INPUT_BG;
        v.inactive.bg_fill = INPUT_BG;
        v.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.hovered.weak_bg_fill = DEEP_RED;
        v.hovered.bg_fill = DEEP_RED;
        v.hovered.bg_stroke = Stroke::new(1.0, BRAND_RED);
        v.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.active.weak_bg_fill = PRESSED_MAROON;
        v.active.bg_fill = PRESSED_MAROON;
        v.active.bg_stroke = Stroke::new(1.0, BRAND_RED);
        v.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        button_core(ui, label, 36.0)
    })
    .inner
}

/// Ghost: transparent / hover DEEP_RED / #2A0B05, TEXT_SECONDARY label.
pub fn ghost_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.inactive.bg_fill = Color32::TRANSPARENT;
        v.inactive.bg_stroke = Stroke::NONE;
        v.inactive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
        v.hovered.weak_bg_fill = DEEP_RED;
        v.hovered.bg_fill = DEEP_RED;
        v.hovered.bg_stroke = Stroke::NONE;
        v.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.active.weak_bg_fill = PRESSED_MAROON;
        v.active.bg_fill = PRESSED_MAROON;
        v.active.bg_stroke = Stroke::NONE;
        v.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        button_core(ui, label, 36.0)
    })
    .inner
}

/// Destructive: transparent + 1 px ERROR / #3A1210 / #541712, ERROR text.
pub fn danger_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.inactive.bg_fill = Color32::TRANSPARENT;
        v.inactive.bg_stroke = Stroke::new(1.0, ERROR);
        v.inactive.fg_stroke = Stroke::new(1.0, ERROR);
        v.hovered.weak_bg_fill = Color32::from_rgb(0x3A, 0x12, 0x10);
        v.hovered.bg_fill = Color32::from_rgb(0x3A, 0x12, 0x10);
        v.hovered.bg_stroke = Stroke::new(1.0, ERROR);
        v.hovered.fg_stroke = Stroke::new(1.0, ERROR);
        v.active.weak_bg_fill = Color32::from_rgb(0x54, 0x17, 0x12);
        v.active.bg_fill = Color32::from_rgb(0x54, 0x17, 0x12);
        v.active.bg_stroke = Stroke::new(1.0, ERROR);
        v.active.fg_stroke = Stroke::new(1.0, ERROR);
        button_core(ui, label, 36.0)
    })
    .inner
}

// ---------------------------------------------------------------------------
// §7 Inputs
// ---------------------------------------------------------------------------

/// Styled single-line input: INPUT_BG, 1 px BORDER, hover #7A2B19,
/// focus 1.5 px BRAND_RED, 10 px horizontal padding, radius 4.
pub fn text_input(ui: &mut Ui, width: f32, buf: &mut String) -> egui::Response {
    let r = text_input_h(ui, width, buf, None);
    attach_edit_menu(ui, &r, buf);
    r
}

/// Right-click context menu for path/text fields: Select all, Copy,
/// Paste (replace contents), Clear - the load screen must behave like a
/// normal Windows edit box (Sami directive). Paste reads the system
/// clipboard via arboard (same backend egui/eframe uses).
pub fn attach_edit_menu(ui: &mut Ui, response: &egui::Response, buf: &mut String) {
    response.context_menu(|ui| {
        let select_all = ui.button("Select all");
        let copy_all = ui.button("Copy");
        let paste_replace = ui.button("Paste (replace contents)");
        let clear = ui.button("Clear");
        if select_all.clicked() {
            let id = response.id;
            let mut st = egui::TextEdit::load_state(ui.ctx(), id);
            if let Some(mut s) = st.take() {
                s.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::default(),
                    egui::text::CCursor::new(buf.chars().count()),
                )));
                egui::TextEdit::store_state(ui.ctx(), id, s);
            }
        }
        if copy_all.clicked() {
            ui.ctx().copy_text(buf.clone());
        }
        if paste_replace.clicked() {
            if let Ok(mut clip) = arboard::Clipboard::new() {
                if let Ok(text) = clip.get_text() {
                    *buf = text;
                }
            }
        }
        if clear.clicked() {
            buf.clear();
        }
    });
}

/// Styled single-line input with an optional in-field placeholder.
pub fn text_input_h(
    ui: &mut Ui,
    width: f32,
    buf: &mut String,
    hint: Option<&str>,
) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.bg_fill = INPUT_BG;
        v.inactive.weak_bg_fill = INPUT_BG;
        v.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.inactive.rounding = Rounding::same(4.0);
        v.hovered.bg_fill = INPUT_BG;
        v.hovered.weak_bg_fill = INPUT_BG;
        v.hovered.bg_stroke = Stroke::new(1.0, INPUT_HOVER);
        v.hovered.rounding = Rounding::same(4.0);
        v.active.bg_fill = INPUT_BG;
        v.active.weak_bg_fill = INPUT_BG;
        v.active.bg_stroke = Stroke::new(1.5, BRAND_RED);
        v.active.rounding = Rounding::same(4.0);
        let mut te = egui::TextEdit::singleline(buf)
            .desired_width(width)
            .margin(egui::Margin::symmetric(10.0, 8.0));
        if let Some(h) = hint {
            te = te.hint_text(h);
        }
        let r = ui.add(te);
        attach_edit_menu(ui, &r, buf);
        r
    })
    .inner
}

/// Styled single-line input with placeholder text.
pub fn text_input_hint(ui: &mut Ui, width: f32, buf: &mut String, hint: &str) -> egui::Response {
    text_input_h(ui, width, buf, Some(hint))
}

/// Styled multiline area (same input treatment, taller).
pub fn multiline_area(ui: &mut Ui, width: f32, height: f32, buf: &mut String) -> egui::Response {
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals.widgets;
        v.inactive.bg_fill = INPUT_BG;
        v.inactive.weak_bg_fill = INPUT_BG;
        v.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
        v.inactive.rounding = Rounding::same(4.0);
        v.hovered.bg_fill = INPUT_BG;
        v.hovered.weak_bg_fill = INPUT_BG;
        v.hovered.bg_stroke = Stroke::new(1.0, INPUT_HOVER);
        v.hovered.rounding = Rounding::same(4.0);
        v.active.bg_fill = INPUT_BG;
        v.active.weak_bg_fill = INPUT_BG;
        v.active.bg_stroke = Stroke::new(1.5, BRAND_RED);
        v.active.rounding = Rounding::same(4.0);
        ui.add(
            egui::TextEdit::multiline(buf)
                .desired_width(width)
                .desired_rows((height / 16.0).max(2.0) as usize)
                .margin(egui::Margin::symmetric(10.0, 8.0)),
        )
    })
    .inner
}

/// Monospace single-line input (addresses, hashes).
pub fn mono_input(ui: &mut Ui, width: f32, buf: &mut String) -> egui::Response {
    ui.scope(|ui| {
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Body, egui::FontId::monospace(12.5));
        text_input(ui, width, buf)
    })
    .inner
}

/// Disabled input surface (DISABLED_BG / DISABLED_FG / BORDER_SUBTLE).
pub fn disabled_input(ui: &mut Ui, width: f32, text: &str, why: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(13.0),
        DISABLED_FG,
    );
    let h = 34.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(4.0), DISABLED_BG);
    ui.painter().rect_stroke(
        rect,
        Rounding::same(4.0),
        Stroke::new(1.0, BORDER_SUBTLE),
    );
    ui.painter().galley(
        pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        DISABLED_FG,
    );
    resp.on_disabled_hover_text(why)
}

// ---------------------------------------------------------------------------
// §6 Navigation rows
// ---------------------------------------------------------------------------

/// One combined hit target: 192×40 row, icon 20×20 at x=12, label x=44,
/// radius 4, full-width DEEP_RED selection with 3 px BRAND_RED left rail.
#[allow(clippy::too_many_arguments)]
pub fn nav_row(
    ui: &mut Ui,
    icon: Option<&TextureHandle>,
    label: &str,
    badge: Option<usize>,
    selected: bool,
) -> egui::Response {
    let size = Vec2::new(192.0, 40.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label)
    });
    let hovered = resp.hovered() && resp.enabled();
    let fill = if selected || hovered {
        DEEP_RED
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(4.0), fill);
    if selected {
        // 3 px red left rail
        let rail = egui::Rect::from_min_size(rect.left_top(), Vec2::new(3.0, 40.0));
        ui.painter().rect_filled(rail, Rounding::same(2.0), BRAND_RED);
    }
    if resp.has_focus() {
        // keyboard focus: 1 px BRAND_RED inner stroke
        ui.painter()
            .rect_stroke(rect.shrink(1.0), Rounding::same(4.0), Stroke::new(1.0, BRAND_RED));
    }
    // icon 20×20 at x=12
    let icon_pos = pos2(rect.left() + 12.0, rect.center().y - 10.0);
    let icon_rect = egui::Rect::from_min_size(icon_pos, Vec2::splat(20.0));
    if let Some(tex) = icon {
        let tint = if selected || hovered {
            TEXT_PRIMARY
        } else {
            TEXT_MUTED
        };
        egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), Vec2::splat(20.0)))
            .tint(tint)
            .paint_at(ui, icon_rect);
    }
    // label at x=44
    let label_color = if selected || hovered {
        TEXT_PRIMARY
    } else {
        TEXT_SECONDARY
    };
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(if selected { 14.0 } else { 14.0 }),
        label_color,
    );
    ui.painter().galley(
        pos2(rect.left() + 44.0, rect.center().y - galley.size().y / 2.0),
        galley,
        label_color,
    );
    // unread badge
    if let Some(n) = badge.filter(|n| *n > 0) {
        let text = if n > 99 { "99+".to_string() } else { n.to_string() };
        let galley = ui.painter().layout_no_wrap(
            text,
            egui::FontId::proportional(11.0),
            Color32::BLACK,
        );
        let pad = 6.0;
        let w = galley.size().x + pad * 2.0;
        let pill = egui::Rect::from_min_size(
            pos2(rect.right() - w - 10.0, rect.center().y - 9.0),
            Vec2::new(w, 18.0),
        );
        ui.painter()
            .rect_filled(pill, Rounding::same(9.0), BRAND_RED);
        ui.painter().galley(
            pos2(pill.left() + pad, pill.center().y - galley.size().y / 2.0),
            galley,
            Color32::BLACK,
        );
    }
    resp
}

/// TOOLS group label (§6) — micro uppercase muted.
pub fn nav_group_label(ui: &mut Ui, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(192.0, 20.0), Sense::hover());
    ui.painter().galley(
        pos2(rect.left() + 12.0, rect.center().y - 6.0),
        ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(11.0),
            TEXT_MUTED,
        ),
        TEXT_MUTED,
    );
}

/// 1 px divider line spanning the sidebar content width.
pub fn hairline(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(192.0, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::ZERO, BORDER_SUBTLE);
}

// ---------------------------------------------------------------------------
// §7 Tables
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct TableCol {
    pub label: &'static str,
    pub width: f32,
    pub right: bool,
}

impl TableCol {
    pub const fn new(label: &'static str, width: f32, right: bool) -> Self {
        Self { label, width, right }
    }
}

/// 38 px header band, TEXT_SECONDARY 12 px semibold, radius 4.
pub fn table_header(ui: &mut Ui, cols: &[TableCol], total_width: f32) {
    egui::Frame::none()
        .fill(TABLE_HEADER_BG)
        .rounding(Rounding::same(4.0))
        .inner_margin(egui::Margin::ZERO)
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(total_width, 38.0));
            ui.horizontal(|ui| {
                for (i, c) in cols.iter().enumerate() {
                    let w = if i == cols.len() - 1 {
                        (total_width - cols[..i].iter().map(|c| c.width + 12.0).sum::<f32>())
                            .max(c.width)
                    } else {
                        c.width
                    };
                    let galley = ui.painter().layout_no_wrap(
                        c.label.to_string(),
                        egui::FontId::proportional(12.0),
                        TEXT_SECONDARY,
                    );
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::hover());
                    let x = if c.right {
                        rect.right() - galley.size().x - 10.0
                    } else {
                        rect.left() + 10.0
                    };
                    ui.painter().galley(
                        pos2(x, rect.center().y - galley.size().y / 2.0),
                        galley,
                        TEXT_SECONDARY,
                    );
                }
            });
        });
}

/// Paints one 38 px row (alternating PANEL_BG/ROW_ALT, selected DEEP_RED with
/// a 2 px BRAND_RED left rail, 1 px separator) and lays the cells out inside
/// it. Cell drawing is painter-based at absolute positions. Returns the
/// InnerResponse so callers can read clicks and the closure's return value.
pub fn table_row<R>(
    ui: &mut Ui,
    idx: usize,
    selected: bool,
    total_width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let cursor = ui.next_widget_position();
    let rect = egui::Rect::from_min_size(cursor, Vec2::new(total_width, 38.0));
    let fill = if selected {
        DEEP_RED
    } else if idx % 2 == 1 {
        ROW_ALT
    } else {
        PANEL_BG
    };
    ui.painter().rect_filled(rect, Rounding::ZERO, fill);
    if selected {
        let rail = egui::Rect::from_min_size(rect.left_top(), Vec2::new(2.0, 38.0));
        ui.painter().rect_filled(rail, Rounding::ZERO, BRAND_RED);
    }
    let sep = egui::Rect::from_min_max(
        pos2(rect.left(), rect.bottom() - 1.0),
        pos2(rect.right(), rect.bottom()),
    );
    ui.painter()
        .rect_filled(sep, Rounding::ZERO, BORDER_SUBTLE);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), add)
}

/// Painter-based cell text inside a `table_row` child ui.
#[allow(clippy::too_many_arguments)]
pub fn table_cell_text(
    ui: &Ui,
    x: f32,
    width: f32,
    val: &str,
    right: bool,
    color: Color32,
    mono: bool,
) {
    let font = if mono {
        egui::FontId::monospace(12.5)
    } else {
        egui::FontId::proportional(13.0)
    };
    let galley = ui.painter().layout_no_wrap(val.to_string(), font, color);
    let base = ui.max_rect().left_top();
    let row_h = ui.max_rect().height();
    let pos_x = if right {
        base.x + x + width - galley.size().x - 10.0
    } else {
        base.x + x + 10.0
    };
    ui.painter().galley(
        pos2(pos_x, base.y + row_h / 2.0 - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// A plain cell label placed at column offset `x` (widget-based, for rows
/// that need interactive siblings).
pub fn table_cell_widget(ui: &mut Ui, x: f32, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                pos2(ui.max_rect().left() + x, ui.max_rect().top()),
                Vec2::new(width, ui.max_rect().height()),
            )),
        add,
    );
}

/// Full-row invisible click target — call FIRST inside a `table_row` when the
/// row itself is selectable; subsequent content is painter-based.
pub fn row_click(ui: &mut Ui, height: f32) -> egui::Response {
    let size = ui.available_size();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(size.x.max(1.0), height.min(size.y).max(1.0)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "row"));
    let _ = rect;
    resp
}

// ---------------------------------------------------------------------------
// §12.3 remaining helpers
// ---------------------------------------------------------------------------

/// `copyable_monospace` — monospace value; returns true when copy was clicked.
pub fn copyable_monospace(ui: &mut Ui, value: &str) -> bool {
    let mut copied = false;
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(value).monospace().color(TEXT_PRIMARY))
                .wrap(),
        );
        copied = ghost_button(ui, "copy").clicked();
    });
    copied
}

/// Semantic notice kinds.
pub enum Notice {
    Info,
    Success,
    Warning,
    Error,
}

impl Notice {
    fn color(&self) -> Color32 {
        match self {
            Notice::Info => INFO,
            Notice::Success => SUCCESS,
            Notice::Warning => WARNING,
            Notice::Error => ERROR,
        }
    }
}

/// `inline_notice` — bordered semantic result panel (never bare colored text).
pub fn inline_notice(ui: &mut Ui, kind: Notice, title: &str, body: &str) {
    let color = kind.color();
    egui::Frame::none()
        .fill(PANEL_BG)
        .stroke(Stroke::new(1.0, color))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title).strong().color(color),
                ),
            );
            if !body.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(body).size(13.0).color(TEXT_SECONDARY),
                    )
                    .wrap(),
                );
            }
        });
}

/// `empty_state` — intentional empty/loading state inside the table body.
pub fn empty_state(
    ui: &mut Ui,
    icon: Option<&TextureHandle>,
    title: &str,
    body: &str,
) {
    ui.vertical_centered(|ui| {
        ui.add_space(24.0);
        if let Some(tex) = icon {
            ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                tex.id(),
                Vec2::splat(32.0),
            )));
            ui.add_space(8.0);
        }
        ui.add(egui::Label::new(
            egui::RichText::new(title).strong().color(TEXT_SECONDARY),
        ));
        ui.add(
            egui::Label::new(egui::RichText::new(body).size(12.0).color(TEXT_MUTED)).wrap(),
        );
        ui.add_space(24.0);
    });
}

/// Status-bar cluster: 12 px horizontal padding, icon + micro label.
pub fn status_cluster(ui: &mut Ui, icon: Option<&TextureHandle>, label: &str, color: Color32) {
    ui.horizontal(|ui| {
        if let Some(tex) = icon {
            ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(
                tex.id(),
                Vec2::splat(16.0),
            )));
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(label)
                    .size(11.0)
                    .strong()
                    .color(color),
            ),
        );
    });
    ui.add_space(4.0);
}

#[cfg(all(test, feature = "gui"))]
mod tests {
    use super::*;

    /// Sami directive — received-coins color tokens are pinned.
    #[test]
    fn tx_in_green_tokens_match_directive() {
        assert_eq!(TX_IN_GREEN, Color32::from_rgb(0x3F, 0xA3, 0x4D));
        assert_eq!(TX_IN_GREEN_BRIGHT, Color32::from_rgb(0x4C, 0xBB, 0x6C));
        assert_eq!(TX_IN_GREEN_DARK, Color32::from_rgb(0x2E, 0x7D, 0x3B));
    }

    /// §3 palette sanity: brand red + black shell anchors stay exact.
    #[test]
    fn palette_anchors_match_spec() {
        assert_eq!(BRAND_RED, Color32::from_rgb(0xE3, 0x21, 0x05));
        assert_eq!(WINDOW_BG, Color32::from_rgb(0x00, 0x00, 0x00));
        assert_eq!(CONTENT_BG, Color32::from_rgb(0x09, 0x09, 0x09));
        assert_eq!(PANEL_BG, Color32::from_rgb(0x12, 0x12, 0x12));
        assert_eq!(INPUT_BG, Color32::from_rgb(0x1C, 0x1C, 0x1C));
        assert_eq!(BORDER, Color32::from_rgb(0x5B, 0x20, 0x13));
        assert_eq!(DEEP_RED, Color32::from_rgb(0x3D, 0x0E, 0x04));
        assert_eq!(TEXT_PRIMARY, Color32::from_rgb(0xF2, 0xE9, 0xE6));
    }
}
