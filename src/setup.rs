//! First-run engine setup. Everything goes into `<data>/engine` and `<data>/models`, so nothing on
//! the system changes and the uninstaller's "Delete app data" removes it all.
//!
//! Pre-installation ([`detect`], [`preflight`]): find the GPU and pick a PyTorch build, list what
//! will be downloaded, check disk space and that the download servers are reachable.
//!
//! Installation:
//! 1. `uv` (a single-binary Python manager) is downloaded,
//! 2. it installs a private Python and a virtual environment,
//! 3. PyTorch is installed from the index that matches the GPU driver (or CPU),
//! 4. `latex_ocr_server` + pinned dependencies are installed and patched with the fast decoder,
//! 5. the model is downloaded.
//!
//! Post-installation:
//! 6. temporary files are removed,
//! 7. recognition is tested on a sample equation, and
//! 8. Integrand is switched to the new engine.
//!
//! Each step is skipped if it's already done, so "Retry" resumes where it failed.

use crate::engine::Core;
use crate::server::Server;
use crate::settings::{ServerSettings, data_dir};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const PYTHON_VERSION: &str = "3.13";
const MODEL_DIR_NAME: &str = "models--Norm--nougat-latex-base";
const MODEL_BYTES: u64 = 1_397_400_000;
const SCRIPTS: [(&str, &str); 4] = [
    ("fast_decode.py", include_str!("../engine/fast_decode.py")),
    ("patch_server.py", include_str!("../engine/patch_server.py")),
    ("download_model.py", include_str!("../engine/download_model.py")),
    ("requirements.txt", include_str!("../engine/requirements.txt")),
];
const SAMPLE_PNG: &[u8] = include_bytes!("../engine/sample.png");

pub fn engine_dir() -> PathBuf {
    data_dir().join("engine")
}

pub fn venv_python() -> PathBuf {
    let venv = engine_dir().join("venv");
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn marker() -> PathBuf {
    engine_dir().join("installed.json")
}

/// The private engine finished installing (and passed its test).
pub fn private_engine_installed() -> bool {
    venv_python().exists() && marker().exists()
}

pub fn model_present(cache_dir: &str) -> bool {
    let snap = Path::new(cache_dir).join(MODEL_DIR_NAME).join("snapshots");
    std::fs::read_dir(snap)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|d| d.path().join("model.safetensors").is_file())
}

// ---------------------------------------------------------------- system detection

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: String,
    pub gpu: Option<String>,
    pub driver: Option<String>,
    /// PyTorch wheel index suffix, e.g. "cu130"; None = the CPU build.
    pub torch_index: Option<String>,
    /// e.g. "GPU, CUDA 13.0" or "CPU"
    pub accel: String,
    /// Why the GPU won't be used, when there is one (or how to get it used).
    pub note: Option<String>,
    pub torch_bytes: u64,
    /// What setup will download, in order.
    pub items: Vec<Item>,
    pub download_gb: f64,
    /// Space the finished engine takes.
    pub disk_gb: f64,
    /// Space needed while installing (the PyTorch wheel sits next to its unpacked copy).
    pub need_gb: f64,
    pub free_gb: Option<f64>,
    pub private_installed: bool,
    pub model_present: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: &'static str,
    pub name: String,
    pub purpose: String,
    pub bytes: u64,
    /// Already on this computer, so it won't be downloaded again.
    pub present: bool,
}

pub fn detect(cache_dir: &str) -> SystemInfo {
    let (nv, driver, cap) = nvidia();
    let other = if nv.is_none() { other_gpu() } else { None };
    let os = std::env::consts::OS.to_string();
    let choice = choose_torch(&os, nv.as_deref(), driver.as_deref(), cap, other.as_deref());
    let have_model = model_present(cache_dir);
    let installed = private_engine_installed();
    let root = engine_dir();
    let items = vec![
        Item {
            id: "uv",
            name: "uv".into(),
            purpose: "Installs Python and the libraries below into Integrand's folder".into(),
            bytes: 22_000_000,
            present: root.join("uv").join(if cfg!(windows) { "uv.exe" } else { "uv" }).exists(),
        },
        Item {
            id: "python",
            name: format!("Python {PYTHON_VERSION}"),
            purpose: "Runs the recognition server. A private copy; a Python you already have isn't touched".into(),
            bytes: 30_000_000,
            present: venv_python().exists(),
        },
        Item {
            id: "torch",
            name: format!("PyTorch, {} build", choice.short),
            purpose: if choice.index.is_some() {
                "Runs the model on the GPU. Most of its size is NVIDIA's CUDA and cuDNN libraries, which it bundles".into()
            } else {
                "Runs the model on the processor".into()
            },
            bytes: choice.bytes,
            present: installed,
        },
        Item {
            id: "server",
            name: "Recognition server and 8 libraries".into(),
            purpose: "latex-ocr-server, Transformers, tokenizers, Pillow, NumPy, gRPC and others".into(),
            bytes: 150_000_000,
            present: installed,
        },
        Item {
            id: "model",
            name: "Nougat LaTeX model".into(),
            purpose: "The trained network that turns an image into LaTeX (CC BY-NC 4.0, non-commercial use)".into(),
            bytes: MODEL_BYTES,
            present: have_model,
        },
    ];
    let download: u64 = items.iter().filter(|i| !i.present).map(|i| i.bytes).sum();
    // Measured: the cu130 wheel (2.0 GB) unpacks to 2.9 GB; the CPU wheels unpack to about 3x.
    let torch_disk = if choice.index.is_some() { choice.bytes as f64 * 1.45 } else { choice.bytes as f64 * 3.0 };
    let disk = 0.03e9 + 0.07e9 + torch_disk + 0.2e9 + MODEL_BYTES as f64;
    let need = if installed { 0.0 } else { disk + choice.bytes as f64 }
        - if have_model { MODEL_BYTES as f64 } else { 0.0 };
    let round = |b: f64| (b / 1e8).round() / 10.0;
    SystemInfo {
        os,
        gpu: nv.or(other),
        driver,
        torch_index: choice.index,
        accel: choice.accel,
        note: choice.note,
        torch_bytes: choice.bytes,
        items,
        download_gb: round(download as f64),
        disk_gb: round(disk),
        need_gb: round(need.max(0.0)),
        free_gb: free_space_gb(&data_dir()),
        private_installed: installed,
        model_present: have_model,
    }
}

/// (name, driver version, compute capability) of the first NVIDIA GPU.
fn nvidia() -> (Option<String>, Option<String>, Option<f32>) {
    let out = hidden("nvidia-smi")
        .args(["--query-gpu=name,driver_version,compute_cap", "--format=csv,noheader"])
        .output();
    let Ok(out) = out else { return (None, None, None) };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().next().filter(|l| !l.trim().is_empty()) else {
        return (None, None, None);
    };
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    (
        f.first().map(|s| s.to_string()),
        f.get(1).map(|s| s.to_string()),
        f.get(2).and_then(|s| s.parse().ok()),
    )
}

/// The first non-NVIDIA graphics adapter (AMD, Intel, …), for telling the user why it isn't used.
fn other_gpu() -> Option<String> {
    let names: Vec<String> = if cfg!(windows) {
        let out = hidden("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance Win32_VideoController).Name"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).collect()
    } else if cfg!(target_os = "linux") {
        let out = hidden("lspci").output().ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("VGA") || l.contains("3D controller"))
            .filter_map(|l| l.splitn(3, ": ").nth(1).or_else(|| l.rsplit(": ").next()).map(str::to_string))
            .collect()
    } else {
        Vec::new()
    };
    let virtual_ = ["Microsoft Basic", "Microsoft Remote", "Virtual", "Parsec", "Citrix", "VMware", "VirtualBox"];
    names.into_iter().find(|n| !n.is_empty() && !virtual_.iter().any(|v| n.contains(v)))
}

struct TorchChoice {
    /// PyTorch wheel index, e.g. "cu130"; None = CPU.
    index: Option<String>,
    accel: String,
    /// For the download list, e.g. "CUDA 13.0" or "CPU".
    short: String,
    bytes: u64,
    note: Option<String>,
}

const CPU_BYTES: u64 = 250_000_000;

/// PyTorch build for this machine. CUDA 13.0 needs driver ≥ 580; RTX 50-series (compute 12.x)
/// needs CUDA ≥ 12.8; older GPUs run the CUDA 12.6 build on any driver ≥ 528. Everything else
/// (no GPU, AMD, Intel, Apple) gets the CPU build: PyTorch's AMD (ROCm) and Intel (XPU) builds
/// aren't wired into the recognition server yet.
fn choose_torch(os: &str, nv: Option<&str>, driver: Option<&str>, cap: Option<f32>, other: Option<&str>) -> TorchChoice {
    let cpu = |note: String| TorchChoice {
        index: None,
        accel: "CPU".into(),
        short: "CPU".into(),
        bytes: CPU_BYTES,
        note: Some(note),
    };
    if os == "macos" {
        return cpu("Apple GPUs aren't supported yet, so the processor does the recognition.".into());
    }
    let Some(nv) = nv else {
        return cpu(match other {
            // An NVIDIA card without nvidia-smi: the driver isn't installed.
            Some(g) if g.to_ascii_uppercase().contains("NVIDIA") => format!(
                "Install the NVIDIA driver for the {g} from nvidia.com and run setup again to use the GPU.                  Until then the processor does the recognition."
            ),
            Some(g) => format!(
                "Only NVIDIA GPUs are supported, so {g} won't be used and the processor does the recognition. \
                 It still works, just a few seconds slower per equation."
            ),
            None => "No dedicated GPU was found, so the processor does the recognition. \
                     It still works, just a few seconds slower per equation."
                .into(),
        });
    };
    let major = driver.and_then(|d| d.split('.').next()?.parse::<u32>().ok()).unwrap_or(0);
    let blackwell = cap.is_some_and(|c| c >= 12.0);
    let gpu = |idx: &str, cuda: &str, bytes: u64| TorchChoice {
        index: Some(idx.into()),
        accel: format!("GPU, CUDA {cuda}"),
        short: format!("CUDA {cuda}"),
        bytes,
        note: None,
    };
    match major {
        580.. => gpu("cu130", "13.0", 2_000_000_000),
        570.. => gpu("cu128", "12.8", 2_600_000_000),
        528.. if !blackwell => gpu("cu126", "12.6", 2_600_000_000),
        _ => cpu(format!(
            "The {nv} needs NVIDIA driver {} or newer (this one is {}). Update the driver and run setup \
             again to use the GPU; until then the processor does the recognition.",
            if blackwell { 570 } else { 528 },
            driver.unwrap_or("unknown"),
        )),
    }
}

// ---------------------------------------------------------------- pre-installation checks

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: &'static str,
    pub label: String,
    /// "ok" | "warn" | "fail"; a "fail" blocks the install.
    pub state: &'static str,
    pub detail: String,
}

/// Everything that should be true before downloading gigabytes: a supported platform, enough
/// disk space, and the three download servers reachable.
pub fn preflight(info: &SystemInfo) -> Vec<Check> {
    let mut checks = Vec::new();
    let platform_ok = matches!(
        (std::env::consts::OS, std::env::consts::ARCH),
        ("windows" | "linux" | "macos", "x86_64" | "aarch64")
    );
    checks.push(Check {
        id: "os",
        label: "Operating system".into(),
        state: if platform_ok { "ok" } else { "fail" },
        detail: format!("{} {}", os_name(), std::env::consts::ARCH),
    });
    checks.push(Check {
        id: "gpu",
        label: "Graphics".into(),
        state: if info.torch_index.is_some() { "ok" } else { "warn" },
        detail: match (&info.gpu, &info.driver, &info.torch_index) {
            (Some(g), Some(d), Some(_)) => format!("{g}, driver {d}. Recognition runs on the GPU"),
            _ => info.note.clone().unwrap_or_default(),
        },
    });
    checks.push(match info.free_gb {
        Some(free) if free < info.need_gb => Check {
            id: "disk",
            label: "Disk space".into(),
            state: "fail",
            detail: format!(
                "Needs {:.1} GB during setup, but only {free:.1} GB is free on the drive with {}",
                info.need_gb,
                data_dir().display()
            ),
        },
        Some(free) => Check {
            id: "disk",
            label: "Disk space".into(),
            state: "ok",
            detail: if info.need_gb > 0.0 {
                format!("{free:.0} GB free. Setup needs {:.1} GB and the engine keeps {:.1} GB", info.need_gb, info.disk_gb)
            } else {
                format!("{free:.0} GB free. Everything is already downloaded")
            },
        },
        None => Check {
            id: "disk",
            label: "Disk space".into(),
            state: "warn",
            detail: format!("Couldn't check. Setup needs about {:.1} GB", info.need_gb),
        },
    });
    let hosts = [
        ("github.com", "https://github.com/astral-sh/uv/releases"),
        ("download.pytorch.org", "https://download.pytorch.org/whl/"),
        ("huggingface.co", "https://huggingface.co/Norm/nougat-latex-base"),
    ];
    let threads: Vec<_> = hosts
        .iter()
        .map(|(_, url)| {
            let url = url.to_string();
            std::thread::spawn(move || {
                hidden("curl")
                    .args(["-sSI", "-o", if cfg!(windows) { "NUL" } else { "/dev/null" }, "--max-time", "10", &url])
                    .status()
                    .is_ok_and(|s| s.success())
            })
        })
        .collect();
    let down: Vec<&str> = hosts
        .iter()
        .zip(threads)
        .filter_map(|((host, _), t)| (!t.join().unwrap_or(false)).then_some(*host))
        .collect();
    checks.push(Check {
        id: "net",
        label: "Internet".into(),
        state: if down.is_empty() { "ok" } else { "fail" },
        detail: if down.is_empty() {
            "GitHub, PyTorch and Hugging Face are reachable".into()
        } else {
            format!("Can't reach {}. Check the connection, proxy or firewall", down.join(", "))
        },
    });
    checks
}

fn os_name() -> String {
    match std::env::consts::OS {
        "windows" => {
            // Build 22000 and later is Windows 11.
            let build = hidden("cmd")
                .args(["/c", "ver"])
                .output()
                .ok()
                .and_then(|o| {
                    let v = String::from_utf8_lossy(&o.stdout).to_string();
                    v.split('.').nth(2).and_then(|b| b.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok())
                });
            match build {
                Some(b) if b >= 22000 => "Windows 11".into(),
                Some(_) => "Windows 10".into(),
                None => "Windows".into(),
            }
        }
        "macos" => "macOS".into(),
        "linux" => "Linux".into(),
        other => other.into(),
    }
}

#[cfg(windows)]
fn free_space_gb(path: &Path) -> Option<f64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then(|| free as f64 / 1e9)
}

#[cfg(not(windows))]
fn free_space_gb(_: &Path) -> Option<f64> {
    None
}

// ---------------------------------------------------------------- installer

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// "uv" | "python" | "torch" | "server" | "model" | "cleanup" | "verify" | "configure" | "done"
    pub step: String,
    /// "running" | "done" | "skipped" | "error" | "cancelled"
    pub state: String,
    pub message: String,
    pub fraction: Option<f64>,
    pub detail: Option<String>,
}

#[derive(Default)]
pub struct Installer {
    running: AtomicBool,
    cancel: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Installer {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
        }
    }

    /// Run the whole setup on a background thread, reporting through "setup" events.
    pub fn start(self: &Arc<Self>, app: AppHandle, core: Arc<Core>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel.store(false, Ordering::SeqCst);
        let me = self.clone();
        std::thread::spawn(move || {
            let mut job = Job {
                app: app.clone(),
                installer: me.clone(),
                log: None,
                step: String::new(),
            };
            let result = job.run(&core);
            if let Err(e) = result {
                let cancelled = me.cancel.load(Ordering::SeqCst);
                job.log(&format!("FAILED: {e:#}"));
                job.emit(if cancelled { "cancelled" } else { "error" }, &format!("{e:#}"), None, None);
            }
            me.running.store(false, Ordering::SeqCst);
        });
    }
}

struct Job {
    app: AppHandle,
    installer: Arc<Installer>,
    log: Option<std::fs::File>,
    step: String,
}

impl Job {
    fn emit(&self, state: &str, message: &str, fraction: Option<f64>, detail: Option<String>) {
        let p = Progress {
            step: self.step.clone(),
            state: state.into(),
            message: message.into(),
            fraction,
            detail,
        };
        let _ = self.app.emit("setup", &p);
    }

    fn begin(&mut self, step: &str, message: &str) {
        self.step = step.into();
        self.log(&format!("== {step}: {message}"));
        self.emit("running", message, None, None);
    }

    fn finish(&self, state: &str, message: &str) {
        self.emit(state, message, Some(1.0), None);
    }

    fn log(&mut self, line: &str) {
        if let Some(f) = &mut self.log {
            let _ = writeln!(f, "{line}");
        }
    }

    fn check_cancel(&self) -> Result<()> {
        if self.installer.cancel.load(Ordering::SeqCst) {
            bail!("Setup was cancelled");
        }
        Ok(())
    }

    fn run(&mut self, core: &Core) -> Result<()> {
        let root = engine_dir();
        std::fs::create_dir_all(root.join("scripts"))?;
        self.log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("setup.log"))
            .ok();
        self.log(&format!("--- setup started {}", chrono::Local::now().to_rfc3339()));
        for (name, text) in SCRIPTS {
            std::fs::write(root.join("scripts").join(name), text)?;
        }
        let cache_dir = core.settings.read().unwrap().server.cache_dir.clone();
        let info = detect(&cache_dir);
        self.log(&format!("{info:?}"));
        let models = PathBuf::from(&cache_dir);

        // 1. uv
        let uv = root.join("uv").join(if cfg!(windows) { "uv.exe" } else { "uv" });
        self.begin("uv", "Downloading the Python manager (uv)");
        if uv.exists() {
            self.finish("skipped", "Already downloaded");
        } else {
            self.download_uv(&root.join("uv"))?;
            self.finish("done", "Downloaded");
        }
        self.check_cancel()?;

        // 2. Python + venv
        self.begin("python", &format!("Installing a private Python {PYTHON_VERSION}"));
        let py = venv_python();
        if py.exists() {
            self.finish("skipped", "Already installed");
        } else {
            let venv = root.join("venv");
            self.cmd(self.uv_cmd(&uv).args(["venv", "--python", PYTHON_VERSION]).arg(&venv), None)?;
            self.finish("done", &format!("Python {PYTHON_VERSION} ready"));
        }
        self.check_cancel()?;

        // 3. PyTorch
        self.begin("torch", &format!("Installing PyTorch for {}", info.accel));
        if self.python_ok(&py, "import torch") {
            self.finish("skipped", "Already installed");
        } else {
            // Download the wheel ourselves: exact progress, resumable, checksum-verified.
            let index = info.torch_index.clone().unwrap_or_else(|| "cpu".into());
            let wheel = self.download_torch(&index, &root.join("downloads"))?;
            self.emit("running", "Installing PyTorch", None, None);
            let mut c = self.uv_cmd(&uv);
            c.args(["pip", "install", "--python"]).arg(&py).arg(&wheel);
            c.args(["--index-url", &format!("https://download.pytorch.org/whl/{index}")]); // for its dependencies
            self.cmd(&mut c, None)?;
            let _ = std::fs::remove_file(&wheel);
            self.finish("done", "Installed");
        }
        self.check_cancel()?;

        // 4. Recognition server + dependencies + fast-decoder patch
        self.begin("server", "Installing the recognition server");
        let req = root.join("scripts").join("requirements.txt");
        self.cmd(
            self.uv_cmd(&uv).args(["pip", "install", "--python"]).arg(&py).arg("-r").arg(&req),
            None,
        )?;
        self.cmd(
            self.uv_cmd(&uv)
                .args(["pip", "install", "--no-deps", "--python"])
                .arg(&py)
                .arg("latex-ocr-server==0.1.2"),
            None,
        )?;
        self.cmd(
            hidden(&py.to_string_lossy()).arg(root.join("scripts").join("patch_server.py")),
            None,
        )?;
        self.finish("done", "Installed");
        self.check_cancel()?;

        // 5. Model
        self.begin("model", "Downloading the recognition model (1.3 GB)");
        if model_present(&cache_dir) {
            self.finish("skipped", "Already downloaded");
        } else {
            std::fs::create_dir_all(&models)?;
            let mut c = hidden(&py.to_string_lossy());
            c.arg(root.join("scripts").join("download_model.py")).arg(&models);
            self.cmd(&mut c, Some((models.clone(), MODEL_BYTES)))?;
            self.finish("done", "Downloaded");
        }
        self.check_cancel()?;

        // Post-installation: clean up, test, then switch Integrand over to the new engine.

        // 6. Remove what was only needed while installing: uv's package cache, the downloaded
        //    wheel, and PyTorch's C++ headers and import libraries (only used to compile
        //    extensions, never at runtime).
        self.begin("cleanup", "Removing temporary files");
        let before = dir_size(&root);
        let _ = std::fs::remove_dir_all(root.join("cache"));
        let _ = std::fs::remove_dir_all(root.join("downloads"));
        if let Some(torch) = torch_dir(&root.join("venv")) {
            let _ = std::fs::remove_dir_all(torch.join("include"));
            for f in walk(&torch.join("lib")) {
                if f.extension().is_some_and(|e| e == "lib") {
                    let _ = std::fs::remove_file(f);
                }
            }
        }
        let freed = before.saturating_sub(dir_size(&root));
        self.finish("done", &format!("Freed {}", gb(freed)));
        self.check_cancel()?;

        // 7. Test on a sample equation, on a spare port so a running engine isn't involved.
        self.begin("verify", "Testing recognition (loading the model takes about 20 s)");
        let sample = root.join("sample.png");
        std::fs::write(&sample, SAMPLE_PNG)?;
        let latex = self.verify(&py, &cache_dir, &sample)?;
        self.log(&format!("sample → {latex}"));
        self.finish("done", &latex);

        // 8. Switch to the new engine.
        self.begin("configure", "Switching Integrand to the new engine");
        let mut s = core.settings.read().unwrap().clone();
        s.server.python = py.to_string_lossy().into_owned();
        core.update_settings(s)?;
        let disk = dir_size(&root) + if models.starts_with(data_dir()) { dir_size(&models) } else { 0 };
        std::fs::write(
            marker(),
            serde_json::to_string_pretty(&serde_json::json!({
                "python": PYTHON_VERSION, "torch": info.torch_index, "accel": info.accel,
                "installed": chrono::Local::now().to_rfc3339(), "bytes": disk,
            }))?,
        )?;
        core.engine_changed();
        self.finish("done", &format!("Using {}; {} on disk", info.accel, gb(disk)));

        self.step = "done".into();
        self.emit("done", &latex, Some(1.0), Some(gb(disk)));
        Ok(())
    }

    /// Fetch the newest PyTorch wheel for this Python/platform from `index` into `dir`.
    fn download_torch(&mut self, index: &str, dir: &Path) -> Result<PathBuf> {
        let base = format!("https://download.pytorch.org/whl/{index}/torch/");
        let html = hidden("curl")
            .args(["-fsSL", "--retry", "3", &base])
            .output()
            .context("Couldn't reach download.pytorch.org")?;
        let html = String::from_utf8_lossy(&html.stdout);
        let (url, sha) = pick_wheel(&html, &wheel_tag()).ok_or_else(|| anyhow!("No PyTorch build for this computer at {base}"))?;
        let name = url.rsplit('/').next().unwrap_or("torch.whl").replace("%2B", "+");
        let name = name.split('#').next().unwrap_or(&name).to_string();
        self.log(&format!("torch wheel: {url}"));
        std::fs::create_dir_all(dir)?;
        let file = dir.join(&name);
        if file.exists() && sha256_file(&file)? == sha {
            return Ok(file);
        }
        let part = dir.join(format!("{name}.part"));
        let size = content_length(&url).unwrap_or(2_000_000_000);
        // -C - resumes a partial download after a cancel or a dropped connection.
        self.cmd(
            hidden("curl").args(["-fsSL", "--retry", "5", "-C", "-", "-o"]).arg(&part).arg(&url),
            Some((part.clone(), size)),
        )
        .context("Downloading PyTorch failed. Check the internet connection, then retry")?;
        self.emit("running", "Checking the download", None, None);
        let got = sha256_file(&part)?;
        if got != sha {
            let _ = std::fs::remove_file(&part);
            bail!("The PyTorch download was corrupted (checksum mismatch). Retry to download it again");
        }
        std::fs::rename(&part, &file)?;
        Ok(file)
    }

    fn uv_cmd(&self, uv: &Path) -> Command {
        let root = engine_dir();
        let mut c = hidden(&uv.to_string_lossy());
        c.env("UV_PYTHON_INSTALL_DIR", root.join("python"))
            .env("UV_CACHE_DIR", root.join("cache"))
            .env("UV_PYTHON_PREFERENCE", "only-managed") // never touch a system Python
            .env("UV_NO_PROGRESS", "1");
        c
    }

    fn python_ok(&self, py: &Path, code: &str) -> bool {
        py.exists() && hidden(&py.to_string_lossy()).args(["-c", code]).status().is_ok_and(|s| s.success())
    }

    fn download_uv(&mut self, dir: &Path) -> Result<()> {
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => "x86_64-pc-windows-msvc.zip",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc.zip",
            ("macos", "aarch64") => "aarch64-apple-darwin.tar.gz",
            ("macos", "x86_64") => "x86_64-apple-darwin.tar.gz",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu.tar.gz",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu.tar.gz",
            (os, arch) => bail!("Automatic setup isn't available for {os}/{arch}; use your own Python instead"),
        };
        std::fs::create_dir_all(dir)?;
        let archive = dir.join(format!("uv-{target}"));
        let url = format!("https://github.com/astral-sh/uv/releases/latest/download/uv-{target}");
        self.cmd(
            hidden("curl").args(["-fsSL", "--retry", "3", "-o"]).arg(&archive).arg(&url),
            Some((dir.to_path_buf(), 22_000_000)),
        )
        .context("Downloading uv failed. Check the internet connection")?;
        self.cmd(hidden("tar").arg("-xf").arg(&archive).arg("-C").arg(dir), None)?;
        let _ = std::fs::remove_file(&archive);
        // The tar.gz builds unpack into a subfolder.
        let exe = if cfg!(windows) { "uv.exe" } else { "uv" };
        if !dir.join(exe).exists() {
            let found = walk(dir)
                .into_iter()
                .find(|p| p.file_name().is_some_and(|n| n == exe))
                .ok_or_else(|| anyhow!("uv not found in download"))?;
            std::fs::rename(found, dir.join(exe))?;
        }
        Ok(())
    }

    /// Run a command, logging its output; optionally report progress as `watch` grows toward `expected` bytes.
    fn cmd(&mut self, c: &mut Command, watch: Option<(PathBuf, u64)>) -> Result<()> {
        self.check_cancel()?;
        self.log(&format!("$ {c:?}"));
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1");
        let mut child = c.spawn().with_context(|| format!("could not run {:?}", c.get_program()))?;
        crate::platform::tie_to_app(&child);
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        for pipe in [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as _),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        drop(tx);
        *self.installer.child.lock().unwrap() = Some(child);

        let base = watch.as_ref().map(|(dir, _)| dir_size(dir)).unwrap_or(0);
        let mut tail: Vec<String> = Vec::new();
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(line) => {
                    self.log(&line);
                    tail.push(line);
                    if tail.len() > 30 {
                        tail.remove(0);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if last_emit.elapsed() > Duration::from_millis(700) {
                last_emit = Instant::now();
                let (fraction, detail) = match &watch {
                    Some((dir, expected)) => {
                        let got = dir_size(dir).saturating_sub(base);
                        (
                            Some((got as f64 / *expected as f64).min(0.97)),
                            Some(format!("{} of about {}", gb(got), gb(*expected))),
                        )
                    }
                    None => (None, tail.last().cloned()),
                };
                self.emit("running", "", fraction, detail);
            }
        }
        let status = match self.installer.child.lock().unwrap().take() {
            Some(mut c) => c.wait()?,
            None => bail!("Setup was cancelled"),
        };
        self.check_cancel()?;
        if !status.success() {
            let tail: Vec<&String> = tail.iter().filter(|l| !l.trim().is_empty()).collect();
            let last = tail.iter().rev().take(4).rev().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
            bail!("{last}");
        }
        Ok(())
    }

    fn verify(&mut self, py: &Path, cache_dir: &str, sample: &Path) -> Result<String> {
        let cfg = ServerSettings {
            python: py.to_string_lossy().into_owned(),
            port: 50190,
            cache_dir: cache_dir.into(),
            autostart: true,
            cpu: false,
        };
        let sample = sample.to_string_lossy().into_owned();
        let result = tauri::async_runtime::block_on(async move {
            let server = Server::new(cfg);
            let r = async {
                server.wait_ready(Duration::from_secs(300)).await?;
                server.generate_latex(&sample).await
            }
            .await;
            server.stop();
            r
        });
        let latex = result.context("The engine didn't start. See setup.log and server.log in the data folder")?;
        if latex.is_empty() {
            bail!("The engine started but recognized nothing on the test image");
        }
        Ok(latex)
    }
}

/// Wheel filename suffix for this platform and the private Python version.
fn wheel_tag() -> String {
    let py = PYTHON_VERSION.replace('.', "");
    let plat = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "aarch64") => "win_arm64",
        ("windows", _) => "win_amd64",
        ("macos", _) => "macosx_11_0_arm64",
        (_, "aarch64") => "manylinux_2_28_aarch64",
        _ => "manylinux_2_28_x86_64",
    };
    format!("-cp{py}-cp{py}-{plat}.whl")
}

/// From a PyTorch index page, the (url, sha256) of the highest stable torch version with `tag`.
fn pick_wheel(html: &str, tag: &str) -> Option<(String, String)> {
    let version = |url: &str| -> Option<Vec<u32>> {
        let file = url.rsplit('/').next()?;
        let v = file.strip_prefix("torch-")?.split(['%', '+', '-']).next()?;
        v.split('.').map(|p| p.parse().ok()).collect() // rejects dev/rc versions
    };
    html.split("href=\"")
        .skip(1)
        .filter_map(|chunk| chunk.split('"').next())
        .filter(|href| href.contains(tag))
        .filter_map(|href| {
            let (url, frag) = href.split_once("#sha256=")?;
            let url = if url.starts_with("http") {
                url.to_string()
            } else {
                format!("https://download.pytorch.org{url}")
            };
            Some((version(&url)?, url, frag.to_string()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, url, sha)| (url, sha))
}

fn content_length(url: &str) -> Option<u64> {
    let out = hidden("curl").args(["-sIL", url]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<u64>().ok())
        })
        .flatten()
        .last()
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A command that never opens a console window.
pub fn hidden(exe: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// The installed `torch` package inside a venv.
fn torch_dir(venv: &Path) -> Option<PathBuf> {
    let win = venv.join("Lib").join("site-packages").join("torch");
    if win.is_dir() {
        return Some(win);
    }
    std::fs::read_dir(venv.join("lib"))
        .ok()?
        .flatten()
        .map(|d| d.path().join("site-packages").join("torch"))
        .find(|p| p.is_dir())
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { stack.push(p) } else { out.push(p) }
        }
    }
    out
}

fn dir_size(dir: &Path) -> u64 {
    if dir.is_file() {
        return dir.metadata().map(|m| m.len()).unwrap_or(0);
    }
    walk(dir).iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum()
}

fn gb(bytes: u64) -> String {
    if bytes < 1_000_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else {
        format!("{:.1} GB", bytes as f64 / 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_stable_wheel() {
        let html = r#"<a href="https://x/whl/cu130/torch-2.14.1%2Bcu130-cp313-cp313-win_amd64.whl#sha256=aa">a</a>
            <a href="https://x/whl/cu130/torch-2.9.1%2Bcu130-cp313-cp313-win_amd64.whl#sha256=bb">b</a>
            <a href="https://x/whl/cu130/torch-2.15.0.dev20260101%2Bcu130-cp313-cp313-win_amd64.whl#sha256=cc">c</a>
            <a href="https://x/whl/cu130/torch-2.14.1%2Bcu130-cp312-cp312-win_amd64.whl#sha256=dd">d</a>"#;
        let (url, sha) = pick_wheel(html, "-cp313-cp313-win_amd64.whl").unwrap();
        assert!(url.contains("2.14.1") && sha == "aa", "{url}");
    }

    /// Network check against download.pytorch.org: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn torch_index_resolves() {
        let html = String::from_utf8(
            hidden("curl")
                .args(["-fsSL", "https://download.pytorch.org/whl/cu130/torch/"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let (url, sha) = pick_wheel(&html, &wheel_tag()).expect("a wheel");
        let size = content_length(&url).expect("content-length");
        println!(
            "{url}
{sha}
{size}"
        );
        assert!(size > 1_000_000_000 && sha.len() == 64);
    }

    /// Live hardware and network check on this computer: `cargo test preflight_here -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn preflight_here() {
        println!("other GPU: {:?}", other_gpu());
        let info = detect(&data_dir().join("models").to_string_lossy());
        println!("{info:#?}");
        for c in preflight(&info) {
            println!("{c:?}");
        }
    }

    #[test]
    fn torch_choice() {
        let nv = |driver: &str, cap: f32| choose_torch("windows", Some("RTX"), Some(driver), Some(cap), None);
        assert_eq!(nv("582.05", 12.0).index.as_deref(), Some("cu130"));
        assert_eq!(nv("572.16", 12.0).index.as_deref(), Some("cu128"));
        assert_eq!(nv("552.22", 8.6).index.as_deref(), Some("cu126"));
        let old = nv("552.22", 12.0); // RTX 50 on an old driver
        assert!(old.index.is_none() && old.note.unwrap().contains("570"));
        let no_driver = choose_torch("windows", None, None, None, Some("NVIDIA GeForce GTX 1660"));
        assert!(no_driver.note.unwrap().starts_with("Install the NVIDIA driver"));
        let amd = choose_torch("windows", None, None, None, Some("AMD Radeon RX 7800 XT"));
        assert!(amd.index.is_none() && amd.note.unwrap().contains("Radeon"));
        assert!(choose_torch("windows", None, None, None, None).index.is_none());
        assert!(choose_torch("macos", None, None, None, None).index.is_none());
    }
}
