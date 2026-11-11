//! Tray icon and its popover (design 2i). Left-click opens the popover above
//! the icon; right-click shows a small native menu as a fallback.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Rect};

use crate::commands::tray::show_main;

pub const LABEL: &str = "tray";
const GAP: f64 = 8.0;
const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 470.0;
const UPDATE_BANNER_HEIGHT: f64 = 64.0;

/// Places the popover just above (or below, for a top taskbar) the icon,
/// kept inside the monitor's work area.
fn place(app: &AppHandle, rect: Rect) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let icon = rect.position.to_physical::<f64>(scale);
    let icon_size = rect.size.to_physical::<f64>(scale);
    let Ok(size) = window.outer_size() else {
        return;
    };
    let (width, height) = (size.width as f64, size.height as f64);
    let monitor = app
        .monitor_from_point(icon.x, icon.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    let (mut x, mut y) = (
        icon.x + icon_size.width / 2.0 - width / 2.0,
        icon.y - height - GAP * scale,
    );
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let (left, top) = (area.position.x as f64, area.position.y as f64);
        let (right, bottom) = (left + area.size.width as f64, top + area.size.height as f64);
        if y < top {
            y = icon.y + icon_size.height + GAP * scale;
        }
        x = x.clamp(left + GAP, (right - width - GAP).max(left));
        y = y.clamp(top + GAP, (bottom - height - GAP).max(top));
    }
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn toggle_popover(app: &AppHandle, rect: Rect) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let _ = app.emit_to(LABEL, "tray-refresh", ());
    let height = if crate::commands::update::pending_update(app.clone()).is_some() {
        HEIGHT + UPDATE_BANNER_HEIGHT
    } else {
        HEIGHT
    };
    let _ = window.set_size(LogicalSize::new(WIDTH, height));
    place(app, rect);
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn install(app: &App) -> tauri::Result<()> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "History…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Yap", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&history, &settings, &quit])?;
    let mut builder = TrayIconBuilder::with_id("yap");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder
        .tooltip("Yap")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                let _ = show_main(app, "settings");
            }
            "history" => {
                let _ = show_main(app, "history");
            }
            "quit" => crate::shutdown(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    Ok(())
}
