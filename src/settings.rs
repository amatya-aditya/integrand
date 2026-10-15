//! User settings, stored as JSON in the config dir and edited from the Settings UI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Folder name for settings and data. Matches the bundle identifier so the uninstaller's
/// "Delete app data" option removes everything, including the private engine.
pub const APP_DIR: &str = "com.integrand.desktop";
/// Folder name used by pre-release builds (when the app was called "LaTeX OCR").
const LEGACY_DIR: &str = "latex-ocr-desktop";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server: ServerSettings,
    pub hotkeys: Vec<Hotkey>,
    pub formats: Vec<Format>,
    pub profiles: Vec<Profile>,
    /// Format used when no app profile matches.
    pub default_format: String,
    pub restore_clipboard: bool,
    pub show_hud: bool,
    pub start_at_login: bool,
    pub save_images: bool,
    pub history_limit: usize,
    /// "system" | "light" | "dark"
    pub theme: String,
    pub onboarded: bool,
    /// Bumped when defaults change in a way existing settings should pick up (see `normalize`).
    /// A missing field means an old file (0), not the current version.
    #[serde(default)]
    pub version: u32,
}

pub const SETTINGS_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerSettings {
    /// Python interpreter that has `latex_ocr_server` installed.
    pub python: String,
    pub port: u16,
    /// Hugging Face model cache (--cache_dir); empty = ~/.cache/huggingface.
    pub cache_dir: String,
    /// Start the server when nothing is listening on `port`.
    pub autostart: bool,
    pub cpu: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    /// Open the screen snipper, OCR the selection, paste into the focused app.
    SnipPaste,
    /// OCR the image already on the clipboard and paste.
    ClipboardPaste,
    /// Snip and copy the result without pasting.
    SnipCopy,
    /// OCR the clipboard image and copy the result.
    ClipboardCopy,
    OpenHistory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hotkey {
    pub id: String,
    pub action: Action,
    /// e.g. "Ctrl+Alt+S"
    pub keys: String,
    /// Format id, or "auto" to pick by the focused app's profile.
    #[serde(default = "auto")]
    pub format: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Format {
    pub id: String,
    pub name: String,
    /// Placeholders: {latex} {latex_str} {latex_sq} {mathml} {latex_wl} {latex_wl_str}
    pub template: String,
    #[serde(default)]
    pub builtin: bool,
    /// Keys pressed before / after pasting, e.g. "Ctrl+4" and "Enter" (comma-separated; "wait 200" pauses).
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// Executable name, e.g. "WINWORD.EXE" (case-insensitive).
    pub app: String,
    /// Display name, e.g. "Microsoft Word".
    pub name: String,
    pub format: String,
    /// Full executable path when known (used for its icon).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

fn auto() -> String {
    "auto".into()
}
fn yes() -> bool {
    true
}

pub fn builtin_formats() -> Vec<Format> {
    let f = |id: &str, name: &str, template: &str| Format {
        id: id.into(),
        name: name.into(),
        template: template.into(),
        builtin: true,
        before: String::new(),
        after: String::new(),
    };
    vec![
        // Mathematica's Inline TeX Input (Ctrl+4 … Enter) typesets the formula in the notebook.
        Format {
            before: "Ctrl+4".into(),
            after: "Enter".into(),
            ..f("mathematica-typeset", "Mathematica (typeset)", "{latex_clean}")
        },
        f("latex", "LaTeX", "{latex}"),
        f("inline", "Inline math  $…$", "${latex}$"),
        f("display", "Display math  $$…$$", "$${latex}$$"),
        f("mathml", "Word / PowerPoint equation", "{mathml}"),
        f(
            "mathematica",
            "Mathematica (ToExpression)",
            "ToExpression[\"{latex_wl_str}\", TeXForm]",
        ),
        f("sympy", "Python (SymPy)", "parse_latex(r\"{latex}\")"),
        f("matplotlib", "Python (matplotlib label)", "r\"${latex}$\""),
        f("matlab", "MATLAB (latex interpreter)", "'${latex_sq}$'"),
    ]
}

fn builtin_profiles() -> Vec<Profile> {
    let p = |app: &str, name: &str, format: &str| Profile {
        app: app.into(),
        name: name.into(),
        format: format.into(),
        path: String::new(),
    };
    if cfg!(target_os = "macos") {
        return vec![
            p("Mathematica", "Mathematica", "mathematica-typeset"),
            p("Microsoft Word", "Microsoft Word", "mathml"),
            p("Microsoft PowerPoint", "Microsoft PowerPoint", "mathml"),
            p("MATLAB", "MATLAB", "matlab"),
            p("Obsidian", "Obsidian", "inline"),
        ];
    }
    vec![
        p("WolframNB.exe", "Mathematica", "mathematica-typeset"),
        p("Mathematica.exe", "Mathematica", "mathematica-typeset"),
        p("WINWORD.EXE", "Microsoft Word", "mathml"),
        p("POWERPNT.EXE", "Microsoft PowerPoint", "mathml"),
        p("ONENOTE.EXE", "OneNote", "mathml"),
        p("OUTLOOK.EXE", "Outlook", "mathml"),
        p("MATLAB.exe", "MATLAB", "matlab"),
        p("Obsidian.exe", "Obsidian", "inline"),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        let hk = |id: &str, action, keys: &str| Hotkey {
            id: id.into(),
            action,
            keys: keys.into(),
            format: auto(),
            enabled: true,
        };
        Self {
            server: ServerSettings::default(),
            hotkeys: vec![
                hk("snip", Action::SnipPaste, "Ctrl+Alt+S"),
                hk("clipboard", Action::ClipboardPaste, "Ctrl+Alt+L"),
                hk("history", Action::OpenHistory, "Ctrl+Alt+H"),
            ],
            formats: builtin_formats(),
            profiles: builtin_profiles(),
            default_format: "latex".into(),
            restore_clipboard: false,
            show_hud: true,
            start_at_login: true,
            save_images: true,
            history_limit: 500,
            theme: "system".into(),
            onboarded: false,
            version: SETTINGS_VERSION,
        }
    }
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            // The private engine installed by the setup wizard (see setup.rs).
            python: crate::setup::venv_python().to_string_lossy().into_owned(),
            port: 50051,
            cache_dir: default_model_dir(),
            autostart: true,
            cpu: false,
        }
    }
}

impl Settings {
    pub fn format(&self, id: &str) -> Option<&Format> {
        self.formats.iter().find(|f| f.id == id)
    }

    /// Format for an output: explicit id, else the focused app's profile, else the default.
    pub fn resolve_format(&self, requested: &str, app_exe: Option<&str>) -> (Format, Option<&Profile>) {
        let profile = app_exe.and_then(|exe| self.profiles.iter().find(|p| p.app.eq_ignore_ascii_case(exe)));
        let id = match requested {
            "auto" | "" => profile.map(|p| p.format.as_str()).unwrap_or(&self.default_format),
            id => id,
        };
        let fmt = self
            .format(id)
            .or_else(|| self.format("latex"))
            .cloned()
            .unwrap_or_else(|| builtin_formats().remove(0));
        (fmt, profile)
    }

    /// Fill in anything missing after an upgrade (new built-in formats) and fix dangling references.
    fn normalize(&mut self) {
        // 1.0 pre-release names
        for f in self.formats.iter_mut().filter(|f| f.builtin) {
            if let Some(b) = builtin_formats().into_iter().find(|b| b.id == f.id && f.name.contains(" · ")) {
                f.name = b.name;
            }
        }
        for b in builtin_formats() {
            if !self.formats.iter().any(|f| f.id == b.id) {
                self.formats.push(b);
            }
        }
        if self.version < 2 {
            // v2: Mathematica pastes typeset math via Inline TeX Input; ToExpression uses cleaned LaTeX.
            for f in self.formats.iter_mut().filter(|f| f.id == "mathematica" && f.builtin) {
                if f.template == "ToExpression[\"{latex_str}\", TeXForm]" {
                    f.template = "ToExpression[\"{latex_wl_str}\", TeXForm]".into();
                }
                if f.name == "Mathematica" {
                    f.name = "Mathematica (ToExpression)".into();
                }
            }
            for p in self.profiles.iter_mut().filter(|p| p.format == "mathematica") {
                p.format = "mathematica-typeset".into();
            }
            self.version = 2;
        }
        if self.version < 3 {
            // v3: typeset Mathematica keeps the math as written (pretty e^x); ToExpression does the semantic rewrite.
            for f in self
                .formats
                .iter_mut()
                .filter(|f| f.id == "mathematica-typeset" && f.template == "{latex_wl}")
            {
                f.template = "{latex_clean}".into();
            }
            self.version = SETTINGS_VERSION;
        }
        if self.history_limit == 0 {
            self.history_limit = 500;
        }
        if self.server.cache_dir.trim().is_empty() {
            self.server.cache_dir = default_model_dir();
        }
    }
}

/// The app's own model folder.
pub fn default_model_dir() -> String {
    data_dir().join("models").to_string_lossy().into_owned()
}

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir().unwrap_or_else(std::env::temp_dir).join(APP_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn data_dir() -> PathBuf {
    let dir = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join(APP_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let path = settings_path();
    let mut s = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::error!("{}: {e}; using defaults", path.display());
            let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            Settings::default()
        }),
        Err(_) => first_run_defaults(),
    };
    s.normalize();
    s
}

pub fn save(s: &Settings) -> Result<()> {
    let path = settings_path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
    std::fs::rename(&tmp, &path).with_context(|| format!("saving {}", path.display()))
}

fn first_run_defaults() -> Settings {
    Settings::default()
}

/// One-time move of pre-release data ("LaTeX OCR" builds) to the new folders, rewriting the
/// absolute paths stored in settings and history. Retries and falls back to moving item by item
/// (merging into the new folder) when a file is briefly locked, e.g. by a virus scanner.
pub fn migrate_legacy_dirs() {
    let all: Vec<(PathBuf, PathBuf)> = [dirs::config_dir(), dirs::data_local_dir()]
        .into_iter()
        .flatten()
        .map(|d| (d.join(LEGACY_DIR), d.join(APP_DIR)))
        .collect();
    if !all.iter().any(|(old, _)| old.is_dir()) {
        return;
    }
    let mut notes = Vec::new();
    for (old, new) in all.iter().filter(|(old, _)| old.is_dir()) {
        if let Err(e) = move_dir(old, new) {
            notes.push(format!("moving {} → {}: {e}", old.display(), new.display()));
        }
    }
    // Paths in either file can point into either folder (settings in Roaming name the model in Local).
    for (_, new) in &all {
        for file in ["settings.json", "history.json"] {
            let path = new.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            for (o, n) in &all {
                replace_prefix(&mut v, &o.to_string_lossy(), &n.to_string_lossy());
            }
            if let Ok(text) = serde_json::to_string_pretty(&v) {
                let _ = std::fs::write(&path, text);
            }
        }
    }
    if let Some((_, new)) = all.last() {
        let _ = std::fs::create_dir_all(new);
        let _ = std::fs::write(
            new.join("migration.log"),
            if notes.is_empty() {
                "ok
"
                .to_string()
            } else {
                notes.join(
                    "
",
                )
            },
        );
    }
}

/// Move `old` to `new`; if `new` already exists (or the rename keeps failing), merge item by item.
/// `old` is removed only once everything has been moved out of it.
fn move_dir(old: &Path, new: &Path) -> std::io::Result<()> {
    for _ in 0..5 {
        if new.exists() {
            break;
        }
        if std::fs::rename(old, new).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    std::fs::create_dir_all(new)?;
    let mut result = Ok(());
    for entry in std::fs::read_dir(old)?.flatten() {
        let (src, dst) = (entry.path(), new.join(entry.file_name()));
        let r = if src.is_dir() && dst.is_dir() {
            move_dir(&src, &dst)
        } else if dst.exists() {
            Ok(()) // keep the newer file; the old one stays behind
        } else {
            std::fs::rename(&src, &dst)
        };
        if r.is_err() {
            result = r;
        }
    }
    let _ = std::fs::remove_dir(old); // only succeeds when empty
    result
}

fn replace_prefix(v: &mut serde_json::Value, old: &str, new: &str) {
    match v {
        serde_json::Value::String(s) if s.starts_with(old) => *s = format!("{new}{}", &s[old.len()..]),
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| replace_prefix(x, old, new)),
        serde_json::Value::Object(o) => o.values_mut().for_each(|x| replace_prefix(x, old, new)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_by_profile() {
        let s = Settings::default();
        // Default profiles are keyed by exe name on Windows/Linux and by app name on macOS.
        let word = if cfg!(target_os = "macos") { "Microsoft Word" } else { "WINWORD.EXE" };
        assert_eq!(s.resolve_format("auto", Some(&word.to_lowercase())).0.id, "mathml");
        assert_eq!(s.resolve_format("auto", Some("notepad.exe")).0.id, "latex");
        assert_eq!(s.resolve_format("inline", Some(word)).0.id, "inline");
        assert_eq!(s.resolve_format("missing", None).0.id, "latex");
    }

    #[test]
    fn roundtrip() {
        let s = Settings::default();
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
        // older/partial files still load
        let mut partial: Settings = serde_json::from_str(
            r#"{"showHud": false, "profiles": [{"app": "WolframNB.exe", "name": "Mathematica", "format": "mathematica"}]}"#,
        )
        .unwrap();
        assert_eq!(partial.version, 0);
        partial.normalize();
        assert_eq!(partial.profiles[0].format, "mathematica-typeset");
        assert_eq!(partial.version, SETTINGS_VERSION);
        assert!(!partial.show_hud && partial.hotkeys.len() == 3);
    }
}
