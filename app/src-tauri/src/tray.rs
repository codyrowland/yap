//! Tray icon and its popover (design 2i). Left-click opens the popover above
//! the icon; right-click shows a small native menu as a fallback.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, PhysicalPosition, Rect};

use crate::commands::tray::show_main;

pub const LABEL: &str = "tray";
const GAP: f64 = 8.0;

/// A 32×32 microphone glyph in the accent colour, drawn at runtime so the
/// app needs no icon asset for the tray.
fn icon() -> Image<'static> {
    const SIZE: usize = 32;
    let mut rgba = vec![0_u8; SIZE * SIZE * 4];
    let accent = [0xB6, 0x82, 0x35, 0xFF];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Capsule body.
            let in_body = {
                let (cx, top, bottom, radius) = (16.0, 9.0, 16.0, 5.0);
                let dy = if fy < top {
                    top - fy
                } else if fy > bottom {
                    fy - bottom
                } else {
                    0.0
                };
                ((fx - cx).powi(2) + dy.powi(2)).sqrt() <= radius
            };
            // Holder arc, stem and base.
            let distance = ((fx - 16.0).powi(2) + (fy - 16.0).powi(2)).sqrt();
            let in_arc = fy >= 16.0 && (7.5..=9.5).contains(&distance);
            let in_stem = (15.0..=17.0).contains(&fx) && (25.0..=28.0).contains(&fy);
            let in_base = (11.0..=21.0).contains(&fx) && (28.0..=30.0).contains(&fy);
            if in_body || in_arc || in_stem || in_base {
                rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&accent);
            }
        }
    }
    Image::new_owned(rgba, SIZE as u32, SIZE as u32)
}

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
    place(app, rect);
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn install(app: &App) -> tauri::Result<()> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "History…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Yap", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&history, &settings, &quit])?;
    TrayIconBuilder::with_id("yap")
        .icon(icon())
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

#[cfg(test)]
mod tests {
    #[test]
    fn tray_icon_has_visible_pixels_inside_the_square() {
        let icon = super::icon();
        assert_eq!((icon.width(), icon.height()), (32, 32));
        let opaque = icon
            .rgba()
            .chunks_exact(4)
            .filter(|px| px[3] == 255)
            .count();
        assert!(opaque > 100 && opaque < 32 * 32 / 2, "{opaque}");
    }
}
