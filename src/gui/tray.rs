//! System-tray support for the wallet GUI (Windows).
//!
//! The legacy wallet had two Window-tab behaviours that this build had
//! stubbed out with "Not supported by this desktop shell":
//!
//!   * "Minimize on close"                 -> the window minimizes, the
//!                                             process AND the node keep running
//!   * "Minimize to the tray instead of the
//!     taskbar"                           -> the window is HIDDEN and the
//!                                             wallet lives in the system tray
//!
//! The first one needs no help from the OS: it is a window minimize, and it
//! already works. The second one needs a real tray icon with a context
//! menu, which is what this module provides.
//!
//! The tray icon is created on the GUI's own thread and its events are
//! polled from the eframe update loop, so no extra message loop is needed:
//! the tray backend's own window is serviced by the OS message pump that
//! eframe already runs.

/// What the user asked the tray to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Show the wallet window again (left click, or "Show Triangles").
    Show,
    /// Quit the wallet for real (the "Exit" menu item).
    Quit,
}

/// The system tray icon and its context menu.
pub struct TrayIcon {
    #[cfg(windows)]
    _icon: ::tray_icon::TrayIcon,
    #[cfg(windows)]
    _menu: Box<::tray_icon::menu::Menu>,
}

impl TrayIcon {
    /// Create the tray icon.
    ///
    /// `icon_rgba` is the icon's pixels (RGBA, row-major) and `size` its
    /// width and height in pixels. The tooltip is what Windows shows when
    /// the pointer rests on the icon.
    ///
    /// Returns None when the platform has no tray support or the icon could
    /// not be created; the wallet then simply keeps its taskbar button.
    #[cfg(windows)]
    pub fn new(icon_rgba: Vec<u8>, size: u32, tooltip: &str) -> Option<Self> {
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
        use tray_icon::Icon;

        let icon = match Icon::from_rgba(icon_rgba, size, size) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("tray: icon rejected: {e}");
                return None;
            }
        };

        // the ids are what the poll loop matches on
        let show = MenuItem::with_id("show", "Show Triangles", true, None);
        let quit = MenuItem::with_id("quit", "Exit", true, None);
        let menu = match Menu::with_items(&[&show, &PredefinedMenuItem::separator(), &quit]) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("tray: menu failed: {e}");
                return None;
            }
        };

        let built = tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip(tooltip)
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build();
        match built {
            Ok(icon) => Some(Self {
                _icon: icon,
                _menu: Box::new(menu),
            }),
            Err(e) => {
                eprintln!("tray: could not create the tray icon: {e}");
                None
            }
        }
    }

    /// No tray support on this platform.
    #[cfg(not(windows))]
    pub fn new(_icon_rgba: Vec<u8>, _size: u32, _tooltip: &str) -> Option<Self> {
        None
    }

    /// Poll for a tray action. Returns None when the user did not touch the
    /// icon.
    #[cfg(windows)]
    pub fn poll(&mut self) -> Option<TrayAction> {
        use tray_icon::menu::MenuEvent;

        // a left click on the icon (not on the menu) shows the window
        while let Some(ev) = tray_icon::TrayIconEvent::receiver().try_recv().ok() {
            if let tray_icon::TrayIconEvent::Click { button, .. } = ev {
                if button == tray_icon::MouseButton::Left {
                    return Some(TrayAction::Show);
                }
            }
        }
        // the context menu items
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            match ev.id().as_ref() {
                "show" => return Some(TrayAction::Show),
                "quit" => return Some(TrayAction::Quit),
                _ => {}
            }
        }
        None
    }

    /// Poll for a tray action. Returns None when the user did not touch the
    /// icon.
    #[cfg(not(windows))]
    pub fn poll(&mut self) -> Option<TrayAction> {
        None
    }
}

/// The wallet's tray icon pixels, taken from the embedded brand mark.
///
/// Returns (rgba, size). The mark is a PNG; decoding it needs the `image`
/// crate, which the GUI already depends on for the rest of its artwork.
pub fn brand_icon() -> Option<(Vec<u8>, u32)> {
    let bytes = crate::gui::app::brand_mark_bytes()?;
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    // The tray icon is drawn into a size x size square, so the source must
    // be square too - a non-square image would be handed to
    // Icon::from_rgba with the wrong byte count and be rejected (or, worse,
    // be drawn wrong). The brand mark is square; if it ever is not, the
    // image is scaled into a square rather than mis-declared.
    let side = w.max(h);
    let rgba = if w == h {
        img.into_raw()
    } else {
        // PAD into a square canvas rather than stretching it, so the mark
        // keeps its aspect ratio.
        let mut canvas = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
        let ox = (side - w) / 2;
        let oy = (side - h) / 2;
        // copy the mark into the middle of the square
        for y in 0..h {
            for x in 0..w {
                canvas.put_pixel(ox + x, oy + y, *img.get_pixel(x, y));
            }
        }
        canvas.into_raw()
    };
    Some((rgba, side))
}

/// Create the tray icon, if the platform has one.
///
/// The CALLER decides whether a tray icon is wanted: a tray icon that the
/// operator did not ask for is clutter, and the taskbar button is the
/// normal way back into the wallet.
pub fn create_tray() -> Option<TrayIcon> {
    let (rgba, size) = brand_icon()?;
    TrayIcon::new(rgba, size, "Cryptographic Triangles")
}

/// Whether this build/platform has a system tray at all. The Options tab
/// uses it to say honestly why a choice is unavailable instead of offering
/// a checkbox that does nothing.
pub fn platform_has_tray() -> bool {
    cfg!(windows)
}
