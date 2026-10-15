#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Integrand — system-wide equation OCR. Snip (or copy) an equation image, press a hotkey, and the
//! LaTeX — or Mathematica / MathML / … depending on the app you're in — is pasted at the cursor.
//! Recognition runs locally in `latex_ocr_server` (Nougat LaTeX model), set up by the in-app wizard.

mod clip;
mod engine;
mod formats;
mod history;
mod platform;
mod server;
mod settings;
mod setup;
mod tray;

use engine::{Core, HotkeyResult};
use history::{Entry, History};
use serde::Serialize;
use settings::Settings;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::ShortcutState;

type CoreState<'a> = State<'a, Arc<Core>>;
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn main() {
    settings::migrate_legacy_dirs();
    let data = settings::data_dir();
    let _ = simplelog::WriteLogger::init(
        log::LevelFilter::Info,
        {
            let mut c = simplelog::ConfigBuilder::new();
            let _ = c.set_time_offset_to_local();
            c.add_filter_ignore_str("tao").add_filter_ignore_str("h2").build()
        },
        std::fs::File::create(data.join("app.log")).expect("log file"),
    );
    log::info!("Integrand {} starting", env!("CARGO_PKG_VERSION"));
    let minimized = std::env::args().any(|a| a == "--minimized");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app, None)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        app.state::<Arc<Core>>().on_shortcut(shortcut.id());
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_settings,
            suspend_hotkeys,
            set_paused,
            history_list,
            history_delete,
            history_clear,
            history_favorite,
            history_update,
            history_rerun,
            copy_formatted,
            render_template,
            ocr_file,
            ocr_clipboard,
            restart_engine,
            detect_python,
            running_apps,
            open_folder,
            hud_hide,
            set_titlebar,
            engine_info,
            engine_preflight,
            engine_install,
            engine_cancel,
            engine_use_python,
            validate_keys,
            app_icons,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let settings = settings::load();
            let first_run = !settings.onboarded;

            // Re-register every launch so the login entry follows this exe (e.g. after installing or moving it).
            if settings.start_at_login {
                let _ = handle.autolaunch().enable();
            }
            // Persist defaults on first run and any upgrades `settings::load` applied.
            let _ = settings::save(&settings);

            tray::create(&handle)?;
            tray::set_menu_labels(&handle, &settings);
            let core = Core::new(handle.clone(), settings);
            app.manage(core.clone());
            core.register_hotkeys();

            build_hud(&handle)?;
            let main = build_main(&handle)?;
            if !minimized || first_run {
                main.show()?;
                main.set_focus()?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Integrand")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(core) = app.try_state::<Arc<Core>>()
            {
                core.server().stop();
            }
        });
}

fn build_main(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Integrand · LaTeX OCR")
        .inner_size(1180.0, 780.0)
        .min_inner_size(900.0, 600.0)
        .center()
        .visible(false)
        .build()?;
    // Closing the window keeps the app running in the tray.
    let w = win.clone();
    win.on_window_event(move |e| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            let _ = w.hide();
        }
    });
    Ok(win)
}

fn build_hud(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
        .title("Integrand")
        .inner_size(440.0, 150.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focusable(false)
        .focused(false)
        .visible(false)
        .build()
}

pub fn show_main(app: &AppHandle, page: Option<&str>) {
    let Some(win) = app.get_webview_window("main").or_else(|| build_main(app).ok()) else {
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
    if let Some(page) = page {
        let _ = win.emit("navigate", page);
    }
}

/// Bottom-right of the monitor the cursor is on, without taking focus from the paste target.
pub fn show_hud_window(win: &WebviewWindow) {
    let monitor = win
        .cursor_position()
        .ok()
        .and_then(|p| win.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| win.primary_monitor().ok().flatten());
    if let (Some(m), Ok(size)) = (monitor, win.outer_size()) {
        let area = m.work_area();
        let margin = (16.0 * m.scale_factor()) as i32;
        let x = area.position.x + area.size.width as i32 - size.width as i32 - margin;
        let y = area.position.y + area.size.height as i32 - size.height as i32 - margin;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
    // `focusable(false)` makes this a no-activate window, so the paste target keeps focus.
    let _ = win.show();
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStateView {
    settings: Settings,
    status: engine::EngineStatus,
    hotkey_results: Vec<HotkeyResult>,
    paused: bool,
    autostart: bool,
    version: &'static str,
    data_dir: String,
    platform: &'static str,
}

#[tauri::command]
fn get_state(app: AppHandle, core: CoreState) -> AppStateView {
    AppStateView {
        settings: core.settings.read().unwrap().clone(),
        status: core.status(),
        hotkey_results: core.hotkey_results.lock().unwrap().clone(),
        paused: core.paused.load(std::sync::atomic::Ordering::Relaxed),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        version: env!("CARGO_PKG_VERSION"),
        data_dir: settings::data_dir().to_string_lossy().into_owned(),
        platform: std::env::consts::OS,
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, core: CoreState, settings: Settings) -> CmdResult<Vec<HotkeyResult>> {
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(false) != settings.start_at_login {
        let r = if settings.start_at_login {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        r.map_err(err)?;
    }
    core.update_settings(settings).map_err(err)
}

#[tauri::command]
fn suspend_hotkeys(core: CoreState, suspend: bool) {
    core.suspend_hotkeys(suspend);
}

#[tauri::command]
fn set_paused(core: CoreState, paused: bool) {
    core.set_paused(paused);
}

#[tauri::command]
fn history_list(core: CoreState) -> Vec<Entry> {
    core.history.lock().unwrap().entries.clone()
}

#[tauri::command]
fn history_delete(core: CoreState, id: String) {
    core.history.lock().unwrap().delete(&id);
}

#[tauri::command]
fn history_clear(core: CoreState, keep_favorites: bool) {
    core.history.lock().unwrap().clear(keep_favorites);
}

#[tauri::command]
fn history_favorite(core: CoreState, id: String, favorite: bool) {
    let mut h = core.history.lock().unwrap();
    if let Some(e) = h.get_mut(&id) {
        e.favorite = favorite;
    }
    h.persist();
}

#[tauri::command]
fn history_update(core: CoreState, id: String, latex: String) -> CmdResult<Entry> {
    let mut h = core.history.lock().unwrap();
    let e = h.get_mut(&id).ok_or("entry not found")?;
    e.latex = latex;
    let e = e.clone();
    h.persist();
    Ok(e)
}

#[tauri::command]
async fn history_rerun(core: CoreState<'_>, id: String) -> CmdResult<Entry> {
    let image = core
        .history
        .lock()
        .unwrap()
        .get(&id)
        .and_then(|e| e.image.clone())
        .ok_or("this entry has no saved image")?;
    let c = core.inner().clone();
    let t = std::time::Instant::now();
    let latex = tauri::async_runtime::spawn_blocking(move || c.ocr(&image))
        .await
        .map_err(err)?
        .map_err(err)?;
    let mut h = core.history.lock().unwrap();
    let e = h.get_mut(&id).ok_or("entry not found")?;
    e.latex = latex;
    e.ms = t.elapsed().as_millis() as u64;
    let e = e.clone();
    h.persist();
    Ok(e)
}

/// Render `latex` with a format and put it on the clipboard.
#[tauri::command]
fn copy_formatted(core: CoreState, latex: String, format: String) -> CmdResult<String> {
    let s = core.settings.read().unwrap().clone();
    let template = s.format(&format).map(|f| f.template.clone()).unwrap_or_else(|| "{latex}".into());
    let out = formats::render(&template, &latex);
    engine::with_clipboard(&core, |cb| Ok(cb.set_text(out.as_str())?)).map_err(err)?;
    Ok(out)
}

#[tauri::command]
fn render_template(template: String, latex: String) -> String {
    formats::render(&template, &latex)
}

enum Source {
    File(String),
    Clipboard,
}

/// OCR without pasting (drag & drop, "From clipboard" button); copies the default format.
async fn convert(core: &Arc<Core>, source: Source) -> CmdResult<Entry> {
    let c = core.clone();
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<Entry> {
        let s = c.settings.read().unwrap().clone();
        let id = History::new_id();
        let stem = c.history.lock().unwrap().image_path(&id).with_extension("");
        let image = match source {
            Source::File(path) => {
                let ext = std::path::Path::new(&path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("png")
                    .to_ascii_lowercase();
                let dest = stem.with_extension(ext);
                std::fs::copy(&path, &dest)?;
                dest
            }
            Source::Clipboard => engine::with_clipboard(&c, |cb| Ok(clip::image_to_file(cb, &stem)?.0))?,
        };
        let t = std::time::Instant::now();
        let latex = c.ocr(&image.to_string_lossy())?;
        let (fmt, _) = s.resolve_format("auto", None);
        let output = formats::render(&fmt.template, &latex);
        engine::with_clipboard(&c, |cb| Ok(cb.set_text(output.as_str())?))?;
        let entry = Entry {
            id,
            time: chrono::Local::now().to_rfc3339(),
            latex,
            output,
            format: fmt.id,
            app: None,
            app_name: None,
            image: Some(image.to_string_lossy().into_owned()),
            ms: t.elapsed().as_millis() as u64,
            favorite: false,
        };
        c.add_history(entry.clone(), s.history_limit);
        Ok(entry)
    })
    .await
    .map_err(err)?
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn ocr_file(core: CoreState<'_>, path: String) -> CmdResult<Entry> {
    convert(core.inner(), Source::File(path)).await
}

#[tauri::command]
async fn ocr_clipboard(core: CoreState<'_>) -> CmdResult<Entry> {
    convert(core.inner(), Source::Clipboard).await
}

#[tauri::command]
async fn restart_engine(core: CoreState<'_>) -> CmdResult<()> {
    core.restart_server().await.map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PythonCandidate {
    path: String,
    version: String,
    has_server: bool,
}

/// Find Python installs and check which have `latex_ocr_server`.
#[tauri::command]
async fn detect_python() -> Vec<PythonCandidate> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut paths: Vec<String> = Vec::new();
        if cfg!(windows)
            && let Ok(out) = setup::hidden("py").arg("-0p").output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(p) = line.split_whitespace().last().filter(|p| p.ends_with(".exe")) {
                    paths.push(p.to_string());
                }
            }
        }
        paths.extend(["python", "python3"].map(String::from));
        let mut found: Vec<PythonCandidate> = Vec::new();
        for p in paths {
            let probe = "import sys; print(sys.executable); print(sys.version.split()[0]); import latex_ocr_server; print('ok')";
            let Ok(out) = setup::hidden(&p).args(["-c", probe]).output() else {
                continue;
            };
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let (Some(exe), Some(ver)) = (lines.next(), lines.next()) else {
                continue;
            };
            if found.iter().any(|c| c.path.eq_ignore_ascii_case(exe)) {
                continue;
            }
            found.push(PythonCandidate {
                path: exe.to_string(),
                version: ver.to_string(),
                has_server: lines.next() == Some("ok"),
            });
        }
        found.sort_by_key(|c| !c.has_server);
        found
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
async fn engine_info(core: CoreState<'_>) -> CmdResult<setup::SystemInfo> {
    let cache = core.settings.read().unwrap().server.cache_dir.clone();
    tauri::async_runtime::spawn_blocking(move || setup::detect(&cache))
        .await
        .map_err(err)
}

/// Pre-installation checks (platform, GPU, disk space, internet), run before the user clicks Install.
#[tauri::command]
async fn engine_preflight(core: CoreState<'_>) -> CmdResult<Vec<setup::Check>> {
    let cache = core.settings.read().unwrap().server.cache_dir.clone();
    tauri::async_runtime::spawn_blocking(move || setup::preflight(&setup::detect(&cache)))
        .await
        .map_err(err)
}

#[tauri::command]
fn engine_install(app: AppHandle, core: CoreState) {
    core.installer.start(app, core.inner().clone());
}

#[tauri::command]
fn engine_cancel(core: CoreState) {
    core.installer.cancel();
}

/// Use an existing Python that already has `latex_ocr_server`.
#[tauri::command]
async fn engine_use_python(core: CoreState<'_>, path: String) -> CmdResult<()> {
    let p = path.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || setup::hidden(&p).args(["-c", "import latex_ocr_server, torch"]).status())
        .await
        .map_err(err)?
        .is_ok_and(|s| s.success());
    if !ok {
        return Err("That Python doesn't have latex_ocr_server and PyTorch installed. Use the automatic setup instead".into());
    }
    let mut s = core.settings.read().unwrap().clone();
    s.server.python = path;
    core.update_settings(s).map_err(err)?;
    core.engine_changed();
    Ok(())
}

/// Check a "keys before/after paste" sequence; returns a readable error.
#[tauri::command]
fn validate_keys(keys: String) -> CmdResult<()> {
    clip::parse_keys(&keys).map(|_| ()).map_err(|e| e.to_string())
}

/// Icons for programs (by exe name) as PNG data URLs; missing ones are left out.
#[tauri::command]
async fn app_icons(apps: Vec<(String, String)>) -> std::collections::HashMap<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        apps.into_iter()
            .filter_map(|(exe, path)| platform::app_icon_data_url(&exe, &path).map(|url| (exe, url)))
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
fn running_apps() -> Vec<platform::AppInfo> {
    platform::windowed_apps()
}

#[tauri::command]
fn open_folder(which: String) {
    let path = match which.as_str() {
        "config" => settings::config_dir(),
        _ => settings::data_dir(),
    };
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

/// `#rrggbb` → Windows COLORREF (0x00BBGGRR).
#[cfg(windows)]
fn colorref(hex: &str) -> Option<u32> {
    let v = u32::from_str_radix(hex.trim().trim_start_matches('#'), 16).ok()?;
    Some(((v & 0xff) << 16) | (v & 0xff00) | ((v >> 16) & 0xff))
}

#[tauri::command]
fn set_titlebar(window: WebviewWindow, caption: String, text: String, border: String, dark: bool) {
    #[cfg(windows)]
    if let (Ok(hwnd), Some(c), Some(t), Some(b)) = (window.hwnd(), colorref(&caption), colorref(&text), colorref(&border)) {
        platform::set_titlebar(hwnd.0 as isize, c, t, b, dark);
    }
    #[cfg(not(windows))]
    let _ = (window, caption, text, border, dark);
}

#[tauri::command]
fn hud_hide(app: AppHandle) {
    if let Some(w) = app.get_webview_window("hud") {
        let _ = w.hide();
    }
}
