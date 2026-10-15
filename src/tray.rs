//! System tray: logo with a status dot, menu with the main actions.

use crate::engine::{Core, EngineStatus};
use crate::settings::{Action, Settings};
use std::sync::Arc;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "main";
const LOGO_32: &[u8] = include_bytes!("../icons/32x32.png");

struct Items {
    status: MenuItem<Wry>,
    snip: MenuItem<Wry>,
    clipboard: MenuItem<Wry>,
    pause: CheckMenuItem<Wry>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let snip = MenuItem::with_id(app, "snip", "Snip && copy", true, None::<&str>)?;
    let clipboard = MenuItem::with_id(app, "clipboard", "Convert clipboard image", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause hotkeys", true, false, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &snip,
            &clipboard,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "history", "History…", true, None::<&str>)?,
            &MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?,
            &pause,
            &MenuItem::with_id(app, "restart", "Restart OCR engine", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit Integrand", true, None::<&str>)?,
        ],
    )?;
    app.manage(Items {
        status,
        snip,
        clipboard,
        pause,
    });

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_with_dot(None))
        .tooltip("Integrand · LaTeX OCR")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| {
            let core = app.state::<Arc<Core>>();
            match e.id.as_ref() {
                "snip" => core.trigger(Action::SnipCopy, "auto".into()),
                "clipboard" => core.trigger(Action::ClipboardCopy, "auto".into()),
                "history" => crate::show_main(app, Some("history")),
                "settings" => crate::show_main(app, Some("hotkeys")),
                "pause" => core.set_paused(!core.paused.load(std::sync::atomic::Ordering::Relaxed)),
                "restart" => {
                    let core = core.inner().clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = core.restart_server().await {
                            log::error!("restart: {e:#}");
                        }
                    });
                }
                "quit" => {
                    core.server().stop();
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                crate::show_main(tray.app_handle(), None);
            }
        })
        .build(app)?;
    Ok(())
}

/// Hotkeys shown next to the menu actions.
pub fn set_menu_labels(app: &AppHandle, s: &Settings) {
    let Some(items) = app.try_state::<Items>() else { return };
    let keys = |a: Action| {
        s.hotkeys
            .iter()
            .find(|h| h.action == a && h.enabled)
            .map(|h| format!("\t{}", h.keys))
            .unwrap_or_default()
    };
    let _ = items.snip.set_text(format!("Snip && copy{}", keys(Action::SnipPaste)));
    let _ = items
        .clipboard
        .set_text(format!("Convert clipboard image{}", keys(Action::ClipboardPaste)));
}

pub fn update(app: &AppHandle, s: &EngineStatus, paused: bool) {
    let label = match s.state.as_str() {
        "ready" => format!(
            "● Ready{}",
            s.device.as_ref().map(|d| format!(" · {}", d.to_uppercase())).unwrap_or_default()
        ),
        "busy" => "● Recognizing…".into(),
        "snipping" => "● Select an area to convert…".into(),
        "setup" => format!("● {}", s.message),
        "loading" => format!("● {}", s.message),
        "error" => format!("● {}", truncate(&s.message, 60)),
        _ => "● Engine offline".into(),
    };
    if let Some(items) = app.try_state::<Items>() {
        let _ = items.status.set_text(if paused {
            format!("{label} (hotkeys paused)")
        } else {
            label.clone()
        });
        let _ = items.pause.set_checked(paused);
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let color = if paused {
            [148, 163, 184]
        } else {
            match s.state.as_str() {
                "ready" => [34, 197, 94],
                "busy" | "snipping" => [56, 189, 248],
                "loading" | "setup" => [245, 158, 11],
                "error" => [239, 68, 68],
                _ => [148, 163, 184],
            }
        };
        let _ = tray.set_icon(Some(icon_with_dot(Some(color))));
        let _ = tray.set_tooltip(Some(format!("Integrand — {}", label.trim_start_matches("● "))));
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.into()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// The logo with a small status dot in the bottom-right corner.
fn icon_with_dot(color: Option<[u8; 3]>) -> Image<'static> {
    let img = image::load_from_memory(LOGO_32).expect("logo png").to_rgba8();
    let (w, h) = img.dimensions();
    let mut px = img.into_raw();
    if let Some([r, g, b]) = color {
        let (cx, cy, rad) = (w as f32 - 7.0, h as f32 - 7.0, 6.0);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let i = ((y * w + x) * 4) as usize;
                let paint = |px: &mut [u8], c: [u8; 3], cover: f32| {
                    let a = cover.clamp(0.0, 1.0);
                    for k in 0..3 {
                        px[i + k] = (px[i + k] as f32 * (1.0 - a) + c[k] as f32 * a) as u8;
                    }
                    px[i + 3] = px[i + 3].max((a * 255.0) as u8);
                };
                paint(&mut px, [255, 255, 255], rad + 1.6 - d); // ring
                paint(&mut px, [r, g, b], rad - d);
            }
        }
    }
    Image::new_owned(px, w, h)
}
