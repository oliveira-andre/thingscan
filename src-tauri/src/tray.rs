//! The menu bar addon: an icon in the macOS menu bar (the system tray elsewhere) whose panel
//! lists every added device and opens its controls. Right-click for Open / Check / Quit.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Rect, WebviewWindow, WindowEvent};

pub const PANEL: &str = "tray";
pub const MAIN: &str = "main";

/// Space between the menu bar and the panel, and between the panel and the screen edge.
const GAP: f64 = 6.0;
const EDGE: f64 = 8.0;
/// Clicking the icon while the panel is open first blurs the panel (hiding it); a click this
/// soon after means "close", not "open again".
const BLUR_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct PanelState {
    hidden_by_blur: Mutex<Option<Instant>>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Thingscan", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", "Check devices", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Thingscan", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &check, &separator, &quit])?;

    TrayIconBuilder::with_id("thingscan")
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?)
        .icon_as_template(true)
        .tooltip("Thingscan")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app, None),
            // The main window runs the check; every window hears the results.
            "check" => {
                let _ = app.emit_to(MAIN, "check-all", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left, button_state: MouseButtonState::Up, rect, ..
            } = event
            {
                toggle_panel(tray.app_handle(), rect);
            }
        })
        .build(app)?;

    if let Some(panel) = app.get_webview_window(PANEL) {
        let handle = app.clone();
        let window = panel.clone();
        // Like a menu, the panel goes away when you click elsewhere.
        panel.on_window_event(move |event| {
            if let WindowEvent::Focused(false) = event {
                *handle.state::<PanelState>().hidden_by_blur.lock().unwrap() = Some(Instant::now());
                let _ = window.hide();
            }
        });
    }
    Ok(())
}

fn toggle_panel(app: &AppHandle, icon: Rect) {
    let Some(panel) = app.get_webview_window(PANEL) else { return };
    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
        return;
    }
    let just_hidden =
        app.state::<PanelState>().hidden_by_blur.lock().unwrap().is_some_and(|t| t.elapsed() < BLUR_GRACE);
    if just_hidden {
        return;
    }
    let _ = place_under(&panel, icon);
    let _ = panel.show();
    let _ = panel.set_focus();
    let _ = panel.emit("panel-shown", ());
}

/// Centres the panel under the menu bar icon, kept on the icon's screen.
fn place_under(panel: &WebviewWindow, icon: Rect) -> tauri::Result<()> {
    let scale = panel.scale_factor()?;
    let at = icon.position.to_physical::<f64>(scale);
    let icon_size = icon.size.to_physical::<f64>(scale);
    let size = panel.outer_size()?;
    let mut x = at.x + icon_size.width / 2.0 - size.width as f64 / 2.0;
    let y = at.y + icon_size.height + GAP * scale;
    if let Some(monitor) = panel.monitor_from_point(at.x, at.y)? {
        let left = monitor.position().x as f64 + EDGE * scale;
        let right = monitor.position().x as f64 + monitor.size().width as f64 - size.width as f64 - EDGE * scale;
        x = x.clamp(left, right.max(left));
    }
    panel.set_position(PhysicalPosition::new(x, y))
}

/// Brings the main window back (closing it only hides it), optionally on a given view.
pub fn show_main(app: &AppHandle, view: Option<&str>) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    if let Some(main) = app.get_webview_window(MAIN) {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
        if let Some(view) = view {
            let _ = main.emit("navigate", view);
        }
    }
}
