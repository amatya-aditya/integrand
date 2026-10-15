//! OS integration: which app has focus, giving focus back, the screen snipper, clipboard change
//! detection and showing the HUD without stealing focus.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Executable file name, e.g. "WINWORD.EXE" (macOS: application name).
    pub exe: String,
    pub title: String,
    /// Full path of the executable, when known.
    pub path: String,
    #[serde(skip)]
    pub window: isize,
}

#[cfg(windows)]
mod imp {
    use super::AppInfo;
    use image::ImageEncoder;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn exe_path_of_window(hwnd: HWND) -> Option<String> {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == 0 {
            return None;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    fn file_name(path: &str) -> String {
        path.rsplit('\\').next().unwrap_or(path).to_string()
    }

    fn title_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    pub fn foreground_app() -> Option<AppInfo> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            return None;
        }
        let path = exe_path_of_window(hwnd)?;
        Some(AppInfo {
            exe: file_name(&path),
            title: title_of(hwnd),
            path,
            window: hwnd as isize,
        })
    }

    /// Give focus back to the window the user was in (the snipper overlay takes it).
    pub fn focus(app: &AppInfo) {
        let target = app.window as HWND;
        unsafe {
            if GetForegroundWindow() == target || IsWindow(target) == 0 {
                return;
            }
            // SetForegroundWindow is only honoured from the foreground thread's input queue.
            let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
            let me = GetCurrentThreadId();
            AttachThreadInput(me, fg_thread, 1);
            SetForegroundWindow(target);
            AttachThreadInput(me, fg_thread, 0);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    pub fn clipboard_sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// Esc dismisses the snipper overlay without touching the clipboard; treat it as cancel.
    pub fn escape_pressed() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
        unsafe { GetAsyncKeyState(VK_ESCAPE as i32) as u16 & 0x8000 != 0 }
    }

    pub fn launch_snipper() -> anyhow::Result<()> {
        // Windows' built-in region snipper (Win+Shift+S); it puts the selection on the clipboard.
        std::process::Command::new("explorer.exe").arg("ms-screenclip:").spawn()?;
        Ok(())
    }

    /// Colour the native title bar and border (Windows 11) to match the app theme.
    pub fn set_titlebar(window: isize, caption: u32, text: u32, border: u32, dark: bool) {
        use windows_sys::Win32::Graphics::Dwm::*;
        let set = |attr: DWMWINDOWATTRIBUTE, v: u32| unsafe {
            DwmSetWindowAttribute(window as HWND, attr as u32, &v as *const u32 as *const _, 4);
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as u32); // caption button glyphs
        set(DWMWA_CAPTION_COLOR, caption);
        set(DWMWA_TEXT_COLOR, text);
        set(DWMWA_BORDER_COLOR, border);
    }

    /// Visible top-level apps, for picking an app profile.
    pub fn windowed_apps() -> Vec<AppInfo> {
        let skip = [
            "explorer.exe",
            "ApplicationFrameHost.exe",
            "TextInputHost.exe",
            "SystemSettings.exe",
            "integrand.exe",
        ];
        let mut out = all_windowed_apps();
        out.retain(|a| !skip.iter().any(|s| s.eq_ignore_ascii_case(&a.exe)));
        out.sort_by_key(|a| a.exe.to_lowercase());
        out
    }

    fn all_windowed_apps() -> Vec<AppInfo> {
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> i32 {
            let out = unsafe { &mut *(lparam as *mut Vec<AppInfo>) };
            let visible = unsafe { IsWindowVisible(hwnd) } != 0;
            let owned = !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null();
            if visible && !owned {
                let title = title_of(hwnd);
                if !title.is_empty()
                    && let Some(path) = exe_path_of_window(hwnd)
                    && !out.iter().any(|a| a.path.eq_ignore_ascii_case(&path))
                {
                    out.push(AppInfo {
                        exe: file_name(&path),
                        title,
                        path,
                        window: hwnd as isize,
                    });
                }
            }
            1
        }
        let mut out: Vec<AppInfo> = Vec::new();
        unsafe { EnumWindows(Some(cb), &mut out as *mut _ as LPARAM) };
        out
    }

    /// Full path of a program: from a running window, else the "App Paths" registry entry.
    pub fn find_exe_path(exe: &str) -> Option<String> {
        if exe.contains('\\') && std::path::Path::new(exe).exists() {
            return Some(exe.to_string());
        }
        if let Some(a) = all_windowed_apps().into_iter().find(|a| a.exe.eq_ignore_ascii_case(exe)) {
            return Some(a.path);
        }
        use windows_sys::Win32::System::Registry::*;
        let sub: Vec<u16> = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let mut buf = [0u16; 1024];
            let mut len = (buf.len() * 2) as u32;
            let r = unsafe {
                RegGetValueW(
                    root,
                    sub.as_ptr(),
                    std::ptr::null(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr() as *mut _,
                    &mut len,
                )
            };
            if r == 0 {
                let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)])
                    .trim_matches('"')
                    .to_string();
                if std::path::Path::new(&s).exists() {
                    return Some(s);
                }
            }
        }
        None
    }

    /// A program's icon at `size` px, as PNG bytes.
    pub fn icon_png(path: &str, size: i32) -> Option<Vec<u8>> {
        use windows_sys::Win32::Graphics::Gdi::*;
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut hicon: HICON = std::ptr::null_mut();
        let mut id = 0u32;
        let n = unsafe { PrivateExtractIconsW(wide.as_ptr(), 0, size, size, &mut hicon, &mut id, 1, 0) };
        if n == 0 || n == u32::MAX || hicon.is_null() {
            return None;
        }
        let mut px = vec![0u8; (size * size * 4) as usize];
        let ok = unsafe {
            let mut ii: ICONINFO = std::mem::zeroed();
            if GetIconInfo(hicon, &mut ii) == 0 {
                DestroyIcon(hicon);
                return None;
            }
            let hdc = GetDC(std::ptr::null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = size;
            bmi.bmiHeader.biHeight = -size; // top-down rows
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let got = GetDIBits(
                hdc,
                ii.hbmColor,
                0,
                size as u32,
                px.as_mut_ptr() as *mut _,
                &mut bmi,
                DIB_RGB_COLORS,
            );
            // Old-style icons carry no alpha channel: take transparency from the mask.
            if got > 0 && px.chunks_exact(4).all(|p| p[3] == 0) {
                let mut mask = vec![0u8; px.len()];
                GetDIBits(
                    hdc,
                    ii.hbmMask,
                    0,
                    size as u32,
                    mask.as_mut_ptr() as *mut _,
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                for (p, m) in px.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
            ReleaseDC(std::ptr::null_mut(), hdc);
            if !ii.hbmColor.is_null() {
                DeleteObject(ii.hbmColor);
            }
            if !ii.hbmMask.is_null() {
                DeleteObject(ii.hbmMask);
            }
            DestroyIcon(hicon);
            got > 0
        };
        if !ok {
            return None;
        }
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2); // BGRA → RGBA
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&px, size as u32, size as u32, image::ExtendedColorType::Rgba8)
            .ok()?;
        Some(png)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::AppInfo;

    pub fn foreground_app() -> Option<AppInfo> {
        let out = std::process::Command::new("osascript")
            .args([
                "-e",
                "tell application \"System Events\" to get name of first application process whose frontmost is true",
            ])
            .output()
            .ok()?;
        let exe = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!exe.is_empty()).then(|| AppInfo {
            title: exe.clone(),
            exe,
            path: String::new(),
            window: 0,
        })
    }
    pub fn focus(app: &AppInfo) {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &format!("tell application \"{}\" to activate", app.exe)])
            .status();
    }
    pub fn clipboard_sequence() -> u32 {
        0 // the snipper below blocks until done, so no change detection is needed
    }
    pub fn launch_snipper() -> anyhow::Result<()> {
        std::process::Command::new("screencapture").args(["-i", "-c"]).status()?;
        Ok(())
    }
    pub fn escape_pressed() -> bool {
        false
    }
    pub fn windowed_apps() -> Vec<AppInfo> {
        Vec::new()
    }
    pub fn find_exe_path(_: &str) -> Option<String> {
        None
    }
    pub fn icon_png(_: &str, _: i32) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::AppInfo;

    pub fn foreground_app() -> Option<AppInfo> {
        let pid = std::process::Command::new("xdotool")
            .args(["getactivewindow", "getwindowpid"])
            .output()
            .ok()?;
        let pid = String::from_utf8_lossy(&pid.stdout).trim().to_string();
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let exe = exe.file_name()?.to_string_lossy().to_string();
        Some(AppInfo {
            title: exe.clone(),
            exe,
            path: String::new(),
            window: 0,
        })
    }
    pub fn focus(_: &AppInfo) {}
    pub fn clipboard_sequence() -> u32 {
        0
    }
    pub fn launch_snipper() -> anyhow::Result<()> {
        for (cmd, args) in [
            ("flameshot", &["gui", "-c"][..]),
            ("gnome-screenshot", &["-a", "-c"]),
            ("spectacle", &["-r", "-b", "-c"]),
        ] {
            if std::process::Command::new(cmd).args(args).status().is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!("no screenshot tool found (install flameshot)")
    }
    pub fn escape_pressed() -> bool {
        false
    }
    pub fn windowed_apps() -> Vec<AppInfo> {
        Vec::new()
    }
    pub fn find_exe_path(_: &str) -> Option<String> {
        None
    }
    pub fn icon_png(_: &str, _: i32) -> Option<Vec<u8>> {
        None
    }
}

pub use imp::*;

/// A program's icon as a `data:` URL, cached on disk so it still shows when the app isn't running.
pub fn app_icon_data_url(exe: &str, path_hint: &str) -> Option<String> {
    use base64::Engine;
    let name = exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_ascii_lowercase();
    let dir = crate::settings::data_dir().join("icons");
    let cached = dir.join(format!("{name}.png"));
    let png = match std::fs::read(&cached) {
        Ok(b) => b,
        Err(_) => {
            let path = if !path_hint.is_empty() && std::path::Path::new(path_hint).exists() {
                path_hint.to_string()
            } else {
                find_exe_path(exe)?
            };
            let png = icon_png(&path, 64)?;
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&cached, &png);
            png
        }
    };
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// Make a child process exit together with the app (Windows job object), so the engine and
/// setup processes never outlive it and the uninstaller can remove their files.
#[cfg(windows)]
pub fn tie_to_app(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::*;
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job as usize
    });
    unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
}

#[cfg(not(windows))]
pub fn tie_to_app(_: &std::process::Child) {}

/// Whether the snipper call returns before the user has finished selecting (Windows) or blocks (macOS/Linux).
pub const SNIPPER_IS_ASYNC: bool = cfg!(windows);
