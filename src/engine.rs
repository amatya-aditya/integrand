//! App core: shared state, hotkey → job pipeline (snip → OCR → format → paste → history),
//! engine status, tray and HUD updates.

use crate::history::{Entry, History};
use crate::platform::{self, AppInfo};
use crate::server::{Health, Server};
use crate::settings::{Action, Hotkey, Settings};
use crate::{clip, formats};
use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// "offline" | "loading" | "ready" | "snipping" | "busy" | "error"
    pub state: String,
    pub message: String,
    pub device: Option<String>,
    pub port: u16,
    pub last_ms: Option<u64>,
    pub owns_server: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyResult {
    pub id: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Hud {
    /// "ok" | "error" | "working"
    kind: String,
    title: String,
    latex: Option<String>,
    detail: String,
}

struct Job {
    action: Action,
    format: String,
    target: Option<AppInfo>,
}

pub struct Core {
    pub app: AppHandle,
    pub settings: RwLock<Settings>,
    server: RwLock<Arc<Server>>,
    pub history: Mutex<History>,
    pub clipboard: Mutex<Option<arboard::Clipboard>>,
    status: Mutex<EngineStatus>,
    pub paused: AtomicBool,
    cancel: AtomicBool,
    jobs: SyncSender<Job>,
    bindings: Mutex<HashMap<u32, Hotkey>>,
    pub hotkey_results: Mutex<Vec<HotkeyResult>>,
    last_trigger: Mutex<Instant>,
    hud_gen: AtomicU64,
    /// Consecutive engine starts that exited before becoming ready.
    engine_failures: std::sync::atomic::AtomicU32,
    /// Cached "configured Python has latex_ocr_server" check; None = check again.
    engine_ok: Mutex<Option<bool>>,
    pub installer: Arc<crate::setup::Installer>,
}

impl Core {
    pub fn new(app: AppHandle, settings: Settings) -> Arc<Self> {
        let server = new_server(&settings);
        let (tx, rx) = sync_channel::<Job>(1);
        let port = settings.server.port;
        let core = Arc::new(Self {
            app,
            history: Mutex::new(History::load(&crate::settings::data_dir())),
            settings: RwLock::new(settings),
            server: RwLock::new(server),
            clipboard: Mutex::new(arboard::Clipboard::new().ok()),
            status: Mutex::new(EngineStatus {
                state: "offline".into(),
                message: "Starting…".into(),
                device: None,
                port,
                last_ms: None,
                owns_server: false,
            }),
            paused: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            jobs: tx,
            bindings: Mutex::new(HashMap::new()),
            hotkey_results: Mutex::new(Vec::new()),
            last_trigger: Mutex::new(Instant::now() - Duration::from_secs(10)),
            hud_gen: AtomicU64::new(0),
            engine_failures: std::sync::atomic::AtomicU32::new(0),
            engine_ok: Mutex::new(None),
            installer: Arc::new(crate::setup::Installer::default()),
        });
        let c = core.clone();
        std::thread::Builder::new()
            .name("ocr-worker".into())
            .spawn(move || c.worker(rx))
            .unwrap();
        tauri::async_runtime::spawn(core.clone().monitor());
        core
    }

    pub fn server(&self) -> Arc<Server> {
        self.server.read().unwrap().clone()
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut EngineStatus)) {
        let s = {
            let mut s = self.status.lock().unwrap();
            f(&mut s);
            s.clone()
        };
        let _ = self.app.emit("status", &s);
        crate::tray::update(&self.app, &s, self.paused.load(Ordering::Relaxed));
    }

    /// Apply new settings: persist, re-register hotkeys, swap the server if its settings changed.
    pub fn update_settings(&self, new: Settings) -> Result<Vec<HotkeyResult>> {
        crate::settings::save(&new)?;
        let server_changed = self.settings.read().unwrap().server != new.server;
        *self.settings.write().unwrap() = new.clone();
        if server_changed {
            log::info!("server settings changed; reconnecting");
            self.engine_changed();
            let old = std::mem::replace(&mut *self.server.write().unwrap(), new_server(&new));
            old.stop();
            self.set_status(|s| {
                s.port = new.server.port;
                s.state = "loading".into();
                s.device = None;
            });
            let server = self.server();
            tauri::async_runtime::spawn(async move {
                let _ = server.ensure_started().await;
            });
        }
        crate::tray::set_menu_labels(&self.app, &new);
        Ok(self.register_hotkeys())
    }

    pub fn register_hotkeys(&self) -> Vec<HotkeyResult> {
        let gs = self.app.global_shortcut();
        let _ = gs.unregister_all();
        let mut bindings = self.bindings.lock().unwrap();
        bindings.clear();
        let mut results = Vec::new();
        let paused = self.paused.load(Ordering::Relaxed);
        for hk in self
            .settings
            .read()
            .unwrap()
            .hotkeys
            .iter()
            .filter(|h| h.enabled && !h.keys.is_empty())
        {
            let res = hk
                .keys
                .parse::<Shortcut>()
                .map_err(|e| format!("Not a valid shortcut ({e})"))
                .and_then(|sc| {
                    if bindings.contains_key(&sc.id()) {
                        return Err("Used by another Integrand hotkey".into());
                    }
                    if !paused {
                        gs.register(sc).map_err(|_| "Already in use by another app".to_string())?;
                    }
                    Ok(sc)
                });
            match res {
                Ok(sc) => {
                    bindings.insert(sc.id(), hk.clone());
                    results.push(HotkeyResult {
                        id: hk.id.clone(),
                        ok: true,
                        error: None,
                    });
                }
                Err(e) => {
                    log::warn!("hotkey {}: {e}", hk.keys);
                    results.push(HotkeyResult {
                        id: hk.id.clone(),
                        ok: false,
                        error: Some(e),
                    });
                }
            }
        }
        *self.hotkey_results.lock().unwrap() = results.clone();
        results
    }

    /// Temporarily release all hotkeys (while the user records a new one, or "Pause").
    pub fn suspend_hotkeys(&self, suspend: bool) {
        if suspend {
            let _ = self.app.global_shortcut().unregister_all();
        } else if !self.paused.load(Ordering::Relaxed) {
            self.register_hotkeys();
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        self.suspend_hotkeys(paused);
        let s = self.status();
        crate::tray::update(&self.app, &s, paused);
        let _ = self.app.emit("paused", paused);
    }

    pub fn on_shortcut(&self, id: u32) {
        let Some(hk) = self.bindings.lock().unwrap().get(&id).cloned() else {
            return;
        };
        {
            // Key auto-repeat while held would queue duplicate jobs.
            let mut last = self.last_trigger.lock().unwrap();
            if last.elapsed() < Duration::from_millis(350) {
                return;
            }
            *last = Instant::now();
        }
        if hk.action == Action::OpenHistory {
            crate::show_main(&self.app, Some("history"));
            return;
        }
        self.trigger(hk.action, hk.format);
    }

    pub fn trigger(&self, action: Action, format: String) {
        // Remember where the user is typing before a snipper overlay takes focus.
        let target = platform::foreground_app().filter(|a| !a.exe.eq_ignore_ascii_case(OWN_EXE));
        // A newer request cancels a snip that's still waiting for a selection.
        self.cancel.store(true, Ordering::SeqCst);
        match self.jobs.try_send(Job { action, format, target }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => log::info!("busy; ignoring hotkey"),
            Err(TrySendError::Disconnected(_)) => log::error!("worker thread is gone"),
        }
    }

    fn worker(self: Arc<Self>, rx: Receiver<Job>) {
        for job in rx {
            self.cancel.store(false, Ordering::SeqCst);
            self.set_status(|s| s.state = "busy".into());
            let result = self.run_job(&job, Instant::now());
            // Tentatively ready; the monitor corrects this within a tick if the server went away.
            self.set_status(|s| s.state = "ready".into());
            match result {
                Ok(Some((entry, repeated))) => {
                    log::info!(
                        "{} ms → {:?}{}: {}",
                        entry.ms,
                        entry.app,
                        if repeated { " (again)" } else { "" },
                        entry.latex
                    );
                    if !repeated {
                        self.set_status(|s| s.last_ms = Some(entry.ms));
                    }
                    let pasted = matches!(job.action, Action::SnipPaste | Action::ClipboardPaste) && job.target.is_some();
                    let where_ = entry.app_name.clone().or(entry.app.clone()).unwrap_or_else(|| "clipboard".into());
                    let fmt_name = self
                        .settings
                        .read()
                        .unwrap()
                        .format(&entry.format)
                        .map(|f| f.name.clone())
                        .unwrap_or_default();
                    let again = if repeated { " again" } else { "" };
                    self.hud(Hud {
                        kind: "ok".into(),
                        title: if pasted {
                            format!("Pasted{again} into {where_}")
                        } else {
                            format!("Copied{again} to clipboard")
                        },
                        latex: Some(entry.latex.clone()),
                        detail: if repeated {
                            fmt_name
                        } else {
                            format!("{fmt_name}, {} ms", entry.ms)
                        },
                    });
                }
                Ok(None) => {} // cancelled
                Err(e) => {
                    log::warn!("{e:#}");
                    self.hud(Hud {
                        kind: "error".into(),
                        title: "Couldn't convert".into(),
                        latex: None,
                        detail: format!("{e:#}"),
                    });
                }
            }
        }
    }

    /// Ok(None) = cancelled; the bool is true when the previous result was pasted again.
    fn run_job(&self, job: &Job, started: Instant) -> Result<Option<(Entry, bool)>> {
        let settings = self.settings.read().unwrap().clone();
        let snip = matches!(job.action, Action::SnipPaste | Action::SnipCopy);
        let mut t0 = started;
        if snip {
            if !self.wait_for_snip()? {
                return Ok(None);
            }
            t0 = Instant::now(); // don't count the time spent selecting
        } else {
            self.hud(Hud {
                kind: "working".into(),
                title: "Recognizing…".into(),
                latex: None,
                detail: String::new(),
            });
        }

        let id = History::new_id();
        let stem = if settings.save_images {
            self.history.lock().unwrap().image_path(&id).with_extension("")
        } else {
            std::env::temp_dir().join("latex-ocr-current")
        };
        let grabbed = {
            let mut cb = self.clipboard.lock().unwrap();
            let cb = clipboard(&mut cb)?;
            match clip::image_to_file(cb, &stem) {
                Ok(found) => Ok(found),
                // After a paste the clipboard holds our text, not the image. Pressing the hotkey again
                // should paste the same equation again (formatted for wherever the cursor is now).
                Err(e) => match cb.get_text().ok().and_then(|t| self.previous_result(&t)) {
                    Some(prev) if !snip => Err(prev),
                    _ => return Err(e),
                },
            }
        };
        let (latex, image, original, repeated) = match grabbed {
            Ok((image, original)) => {
                if snip {
                    self.hud(Hud {
                        kind: "working".into(),
                        title: "Recognizing…".into(),
                        latex: None,
                        detail: String::new(),
                    });
                }
                (self.ocr(&image.to_string_lossy())?, Some(image), Some(original), false)
            }
            Err(prev) => (prev.latex, prev.image.map(Into::into), None, true),
        };
        let (fmt, profile) = settings.resolve_format(&job.format, job.target.as_ref().map(|a| a.exe.as_str()));
        let output = formats::render(&fmt.template, &latex);
        let paste = matches!(job.action, Action::SnipPaste | Action::ClipboardPaste) && job.target.is_some();
        {
            let mut cb = self.clipboard.lock().unwrap();
            let cb = clipboard(&mut cb)?;
            cb.set_text(output.as_str())?;
            if paste {
                if let Some(target) = &job.target {
                    platform::focus(target);
                }
                // Per-format keys around the paste, e.g. Mathematica's Ctrl+4 … Enter (Inline TeX Input).
                clip::wait_for_modifier_release(Duration::from_secs(3));
                if !fmt.before.trim().is_empty() {
                    clip::send_keys(&fmt.before)?;
                    std::thread::sleep(Duration::from_millis(150));
                }
                clip::paste()?;
                if !fmt.after.trim().is_empty() {
                    std::thread::sleep(Duration::from_millis(200)); // let the app take the paste first
                    clip::send_keys(&fmt.after)?;
                }
                if let (true, Some(original)) = (settings.restore_clipboard, original) {
                    std::thread::sleep(Duration::from_millis(300)); // let the target read the clipboard first
                    clip::restore(cb, original);
                }
            }
        }

        let entry = Entry {
            id,
            time: chrono::Local::now().to_rfc3339(),
            latex,
            output,
            format: fmt.id.clone(),
            app: job.target.as_ref().filter(|_| paste).map(|a| a.exe.clone()),
            app_name: job
                .target
                .as_ref()
                .filter(|_| paste)
                .map(|a| profile.map(|p| p.name.clone()).unwrap_or_else(|| pretty_exe(&a.exe))),
            image: image.filter(|_| settings.save_images).map(|p| p.to_string_lossy().into_owned()),
            ms: t0.elapsed().as_millis() as u64,
            favorite: false,
        };
        if !repeated {
            self.add_history(entry.clone(), settings.history_limit);
        }
        Ok(Some((entry, repeated)))
    }

    /// The history entry whose LaTeX or pasted output is `text` (newest first).
    fn previous_result(&self, text: &str) -> Option<Entry> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let h = self.history.lock().unwrap();
        h.entries
            .iter()
            .find(|e| e.output.trim() == text || e.latex.trim() == text)
            .cloned()
    }

    /// Open the snipper and wait for its selection to land on the clipboard. False = cancelled.
    fn wait_for_snip(&self) -> Result<bool> {
        let before = platform::clipboard_sequence();
        self.set_status(|s| s.state = "snipping".into());
        platform::launch_snipper()?;
        if !platform::SNIPPER_IS_ASYNC {
            self.set_status(|s| s.state = "busy".into());
            return Ok(true);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        std::thread::sleep(Duration::from_millis(150)); // ignore an Esc still held from before
        while Instant::now() < deadline {
            if self.cancel.load(Ordering::SeqCst) || platform::escape_pressed() {
                log::info!("snip cancelled");
                return Ok(false);
            }
            if platform::clipboard_sequence() != before {
                // The snipper may publish the image in several steps; give it a moment.
                std::thread::sleep(Duration::from_millis(80));
                self.set_status(|s| s.state = "busy".into());
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
    }

    /// OCR an image file, starting/waiting for the server if necessary.
    pub fn ocr(&self, image: &str) -> Result<String> {
        let server = self.server();
        let latex = tauri::async_runtime::block_on(async {
            if server.health().await != Health::Ready {
                if !self.engine_available() {
                    crate::show_main(&self.app, None);
                    bail!("The recognition engine isn't set up yet. Finish the setup in the Integrand window");
                }
                self.set_status(|s| s.state = "loading".into());
                self.hud(Hud {
                    kind: "working".into(),
                    title: "Starting OCR engine…".into(),
                    latex: None,
                    detail: "First run after boot takes ~20 s".into(),
                });
                server.wait_ready(Duration::from_secs(180)).await?;
                self.set_status(|s| s.state = "busy".into());
            }
            server.generate_latex(image).await
        })?;
        if latex.is_empty() {
            bail!("No equation recognized");
        }
        Ok(latex)
    }

    pub fn add_history(&self, entry: Entry, limit: usize) {
        self.history.lock().unwrap().add(entry.clone(), limit);
        let _ = self.app.emit("history-added", &entry);
    }

    fn hud(&self, hud: Hud) {
        if !self.settings.read().unwrap().show_hud {
            return;
        }
        let Some(win) = self.app.get_webview_window("hud") else { return };
        let generation = self.hud_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = win.emit("hud", &hud);
        crate::show_hud_window(&win);
        if hud.kind != "working" {
            let app = self.app.clone();
            let core_gen = generation;
            let ms = if hud.kind == "error" { 4500 } else { 2600 };
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(ms));
                let core = app.state::<Arc<Core>>();
                if core.hud_gen.load(Ordering::SeqCst) == core_gen
                    && let Some(w) = app.get_webview_window("hud")
                {
                    let _ = w.hide();
                }
            });
        }
    }

    /// Keep the engine status current; start the server on launch if nobody else has.
    async fn monitor(self: Arc<Self>) {
        // Self-healing: whenever nothing serves the port (a crash, another app's engine closing, first launch),
        // start our own engine. Give up after repeated exits before ever becoming ready.
        let mut spawned_at: Option<Instant> = None;
        let mut ready_since_spawn = false;
        loop {
            let server = self.server();
            let h = server.health().await;
            let owns = server.owns_process();
            let autostart = self.settings.read().unwrap().server.autostart;
            if h == Health::Ready {
                ready_since_spawn = true;
                self.engine_failures.store(0, Ordering::Relaxed);
            }
            let available = if h == Health::Unreachable && !owns {
                let me = self.clone();
                tauri::async_runtime::spawn_blocking(move || me.engine_available())
                    .await
                    .unwrap_or(false)
            } else {
                true
            };
            if h == Health::Unreachable && !owns && !available {
                if self.status().state != "setup" {
                    self.set_status(|s| {
                        s.state = "setup".into();
                        s.message = "Set up the recognition engine to start converting".into();
                    });
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            if h == Health::Unreachable && !owns && autostart {
                if spawned_at.is_some() && !ready_since_spawn {
                    self.engine_failures.fetch_add(1, Ordering::Relaxed);
                }
                let failures = self.engine_failures.load(Ordering::Relaxed);
                let backoff = Duration::from_secs(2 + 15 * failures as u64);
                if failures < 3 && spawned_at.is_none_or(|t| t.elapsed() > backoff) {
                    log::info!("no engine on port {}; starting one", server.port());
                    match server.ensure_started().await {
                        Ok(()) => {
                            spawned_at = Some(Instant::now());
                            ready_since_spawn = false;
                        }
                        Err(e) => self.set_status(|s| {
                            s.state = "error".into();
                            s.message = format!("{e:#}");
                        }),
                    }
                } else if failures >= 3 && self.status().state != "error" {
                    self.set_status(|s| {
                        s.state = "error".into();
                        s.message = "The engine keeps exiting. Check Python and the model folder, then restart it (see server.log).".into();
                    });
                } else {
                    // Don't count the same dead process again while backing off.
                    ready_since_spawn = true;
                }
            }
            let owns = server.owns_process();
            let cur = self.status();
            if !matches!(cur.state.as_str(), "busy" | "snipping") {
                let (state, message) = match h {
                    Health::Ready => (
                        "ready",
                        if owns {
                            "Running"
                        } else {
                            "Using an engine that was already running"
                        },
                    ),
                    Health::Loading => ("loading", "Loading model…"),
                    Health::Unreachable if owns => ("loading", "Starting the engine…"),
                    Health::Unreachable if cur.state == "error" => ("error", cur.message.as_str()),
                    Health::Unreachable if self.installer.is_running() => ("setup", "Setting up the recognition engine…"),
                    Health::Unreachable => ("offline", if autostart { "Starting…" } else { "Not running" }),
                };
                let device = match (h, &cur.device) {
                    (Health::Ready, None) => server.device().await,
                    (Health::Ready, d) => d.clone(),
                    _ => None,
                };
                if cur.state != state
                    || cur.message != message
                    || cur.device != device
                    || cur.owns_server != owns
                    || cur.port != server.port()
                {
                    log::info!("engine: {state} ({message}, device {device:?})");
                    let (state, message) = (state.to_string(), message.to_string());
                    self.set_status(|s| {
                        s.state = state;
                        s.message = message;
                        s.device = device;
                        s.owns_server = owns;
                        s.port = server.port();
                    });
                }
            }
            tokio::time::sleep(if h == Health::Ready {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(500)
            })
            .await;
        }
    }

    /// Whether the configured Python can run the recognition server (cached).
    pub fn engine_available(&self) -> bool {
        if let Some(ok) = *self.engine_ok.lock().unwrap() {
            return ok;
        }
        let py = self.settings.read().unwrap().server.python.clone();
        // find_spec checks both packages are installed without the cost of importing torch.
        let probe = "import importlib.util as u, sys; sys.exit(0 if u.find_spec('latex_ocr_server') and u.find_spec('torch') else 1)";
        let ok = crate::setup::hidden(&py).args(["-c", probe]).status().is_ok_and(|s| s.success());
        log::info!("engine available ({py}): {ok}");
        *self.engine_ok.lock().unwrap() = Some(ok);
        ok
    }

    /// Forget cached engine checks after setup or a settings change.
    pub fn engine_changed(&self) {
        *self.engine_ok.lock().unwrap() = None;
        self.engine_failures.store(0, Ordering::Relaxed);
    }

    pub async fn restart_server(&self) -> Result<()> {
        self.engine_failures.store(0, Ordering::Relaxed);
        let server = self.server();
        server.stop();
        self.set_status(|s| {
            s.state = "loading".into();
            s.message = "Restarting…".into();
        });
        server.wait_ready(Duration::from_secs(180)).await
    }
}

pub const OWN_EXE: &str = if cfg!(windows) { "integrand.exe" } else { "integrand" };

fn new_server(s: &Settings) -> Arc<Server> {
    // The lazy gRPC channel must be created inside the async runtime.
    let cfg = s.server.clone();
    tauri::async_runtime::block_on(async move { Arc::new(Server::new(cfg)) })
}

fn clipboard(cb: &mut Option<arboard::Clipboard>) -> Result<&mut arboard::Clipboard> {
    if cb.is_none() {
        *cb = Some(arboard::Clipboard::new()?);
    }
    cb.as_mut().ok_or_else(|| anyhow!("clipboard unavailable"))
}

pub fn with_clipboard<T>(core: &Core, f: impl FnOnce(&mut arboard::Clipboard) -> Result<T>) -> Result<T> {
    let mut cb = core.clipboard.lock().unwrap();
    f(clipboard(&mut cb)?)
}

/// "WINWORD.EXE" → "Winword"
pub fn pretty_exe(exe: &str) -> String {
    let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(|c| c.to_lowercase())).collect(),
        None => exe.to_string(),
    }
}
