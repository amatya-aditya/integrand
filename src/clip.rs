//! Clipboard image in, text out, and the synthetic paste keystroke.

use anyhow::{Result, anyhow, bail};
use arboard::{Clipboard, ImageData};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use std::path::{Path, PathBuf};
use std::time::Duration;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff"];

/// What was on the clipboard before we replaced it, so it can be restored.
pub enum Original {
    Image(ImageData<'static>),
    Files(Vec<PathBuf>),
}

/// Save the clipboard image as `<dest_stem>.png` (bitmaps from Snipping Tool, screenshots, browsers)
/// or copy a copied image file (e.g. from Explorer) next to it. Returns the written path.
pub fn image_to_file(cb: &mut Clipboard, dest_stem: &Path) -> Result<(PathBuf, Original)> {
    if let Ok(img) = cb.get_image() {
        let path = dest_stem.with_extension("png");
        write_png(&img, &path)?;
        return Ok((path, Original::Image(img)));
    }
    if let Ok(files) = cb.get().file_list() {
        let ext_of = |p: &PathBuf| p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if let Some(src) = files.iter().find(|p| ext_of(p).is_some_and(|e| IMAGE_EXTS.contains(&e.as_str()))) {
            let path = dest_stem.with_extension(ext_of(src).unwrap());
            std::fs::copy(src, &path)?;
            return Ok((path, Original::Files(files)));
        }
    }
    Err(anyhow!("No image on the clipboard"))
}

/// Flatten RGBA onto white and save as RGB PNG (fast compression: latency matters more than size).
fn write_png(img: &ImageData, path: &Path) -> Result<()> {
    let px = img.bytes.chunks_exact(4);
    // Some apps put 32-bit bitmaps with an all-zero alpha channel on the clipboard: treat those as opaque.
    let opaque = px.clone().all(|p| p[3] == 0);
    let rgb: Vec<u8> = px
        .flat_map(|p| {
            let a = if opaque { 255 } else { p[3] as u32 };
            let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
            [blend(p[0]), blend(p[1]), blend(p[2])]
        })
        .collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::NoFilter).write_image(
        &rgb,
        img.width as u32,
        img.height as u32,
        ExtendedColorType::Rgb8,
    )?;
    Ok(())
}

pub fn restore(cb: &mut Clipboard, original: Original) {
    let res = match original {
        Original::Image(img) => cb.set_image(img),
        Original::Files(files) => cb.set().file_list(&files),
    };
    if let Err(e) = res {
        log::warn!("could not restore clipboard: {e}");
    }
}

/// Send the platform paste shortcut to the focused window.
pub fn paste() -> Result<()> {
    // The hotkey's own modifiers may still be held; Ctrl+V would then arrive as e.g. Ctrl+Alt+V.
    wait_for_modifier_release(Duration::from_secs(3));
    send_paste()
}

/// Virtual-key SendInput. (enigo sends scancodes, which many Win32/WinForms controls ignore for Ctrl+V.)
#[cfg(windows)]
fn send_paste() -> Result<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    const VK_V: u16 = 0x56;
    let inputs = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(anyhow!("SendInput blocked (is the target app running as administrator?)"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_paste() -> Result<()> {
    use enigo::{Direction::*, Key, Keyboard};
    let mut enigo = enigo::Enigo::new(&enigo::Settings::default())?;
    let modifier = if cfg!(target_os = "macos") { Key::Meta } else { Key::Control };
    enigo.key(modifier, Press)?;
    let res = enigo.key(Key::Unicode('v'), Click);
    enigo.key(modifier, Release)?;
    Ok(res?)
}

#[cfg(windows)]
pub fn wait_for_modifier_release(timeout: Duration) {
    use std::time::Instant;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    let keys = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN];
    let deadline = Instant::now() + timeout;
    while keys.iter().any(|&k| unsafe { GetAsyncKeyState(k as i32) } as u16 & 0x8000 != 0) {
        if Instant::now() > deadline {
            log::warn!("modifier keys still held; pasting anyway");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(windows))]
pub fn wait_for_modifier_release(_timeout: Duration) {
    // No portable key-state query; give the user a moment to let go of the hotkey.
    std::thread::sleep(Duration::from_millis(150));
}

// ---------------------------------------------------------------- key sequences

/// One step of a key sequence such as `"Ctrl+4, Enter, wait 200, Shift+Enter"`.
#[derive(Debug, PartialEq)]
pub enum KeyStep {
    /// Modifiers (held) and the key, as names: (["Ctrl"], "4")
    Combo(Vec<&'static str>, String),
    Wait(u64),
}

const MODIFIERS: [(&str, &str); 6] = [
    ("ctrl", "Ctrl"),
    ("control", "Ctrl"),
    ("alt", "Alt"),
    ("shift", "Shift"),
    ("win", "Win"),
    ("cmd", "Win"),
];

pub fn parse_keys(seq: &str) -> Result<Vec<KeyStep>> {
    let mut steps = Vec::new();
    for part in seq.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(ms) = part.to_ascii_lowercase().strip_prefix("wait") {
            let ms: u64 = ms
                .trim()
                .trim_end_matches("ms")
                .trim()
                .parse()
                .map_err(|_| anyhow!("\"{part}\": use e.g. wait 200"))?;
            steps.push(KeyStep::Wait(ms.min(5000)));
            continue;
        }
        let mut mods = Vec::new();
        let tokens: Vec<&str> = part.split('+').map(str::trim).collect();
        let (key, mod_tokens) = tokens.split_last().ok_or_else(|| anyhow!("empty key"))?;
        for m in mod_tokens {
            let name = MODIFIERS
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(m))
                .map(|(_, n)| *n)
                .ok_or_else(|| anyhow!("\"{m}\" isn't a modifier (Ctrl, Alt, Shift, Win)"))?;
            mods.push(name);
        }
        if key_code(key).is_none() {
            bail!("\"{key}\" isn't a key name");
        }
        steps.push(KeyStep::Combo(mods, key.to_string()));
    }
    Ok(steps)
}

/// Windows virtual-key code for a key name.
fn key_code(key: &str) -> Option<u16> {
    let k = key.to_ascii_lowercase();
    let named = match k.as_str() {
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "space" => 0x20,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "=" | "plus" => 0xBB,
        "-" | "minus" => 0xBD,
        "," | "comma" => 0xBC,
        "." | "period" => 0xBE,
        "/" => 0xBF,
        ";" => 0xBA,
        "'" => 0xDE,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        "`" => 0xC0,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let b = k.as_bytes();
    match b {
        [c @ b'a'..=b'z'] => Some(c.to_ascii_uppercase() as u16),
        [c @ b'0'..=b'9'] => Some(*c as u16),
        [b'f', rest @ ..] => std::str::from_utf8(rest)
            .ok()?
            .parse::<u16>()
            .ok()
            .filter(|n| (1..=24).contains(n))
            .map(|n| 0x6F + n),
        _ => None,
    }
}

/// Press a key sequence in the focused window.
pub fn send_keys(seq: &str) -> Result<()> {
    for step in parse_keys(seq)? {
        match step {
            KeyStep::Wait(ms) => std::thread::sleep(Duration::from_millis(ms)),
            KeyStep::Combo(mods, key) => {
                press_combo(&mods, &key)?;
                std::thread::sleep(Duration::from_millis(60));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn press_combo(mods: &[&str], key: &str) -> Result<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    let vk = |m: &str| match m {
        "Ctrl" => VK_CONTROL,
        "Alt" => VK_MENU,
        "Shift" => VK_SHIFT,
        _ => VK_LWIN,
    };
    let input = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let code = key_code(key).ok_or_else(|| anyhow!("unknown key {key}"))?;
    let mut seq: Vec<INPUT> = mods.iter().map(|m| input(vk(m), false)).collect();
    seq.push(input(code, false));
    seq.push(input(code, true));
    seq.extend(mods.iter().rev().map(|m| input(vk(m), true)));
    let sent = unsafe { SendInput(seq.len() as u32, seq.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != seq.len() {
        bail!("SendInput blocked");
    }
    Ok(())
}

#[cfg(not(windows))]
fn press_combo(mods: &[&str], key: &str) -> Result<()> {
    use enigo::{Direction::*, Key, Keyboard};
    let mut enigo = enigo::Enigo::new(&enigo::Settings::default())?;
    let m = |name: &str| match name {
        "Ctrl" => Key::Control,
        "Alt" => Key::Alt,
        "Shift" => Key::Shift,
        _ => Key::Meta,
    };
    let k = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        other => Key::Unicode(other.chars().next().unwrap_or(' ')),
    };
    for name in mods {
        enigo.key(m(name), Press)?;
    }
    let r = enigo.key(k, Click);
    for name in mods.iter().rev() {
        enigo.key(m(name), Release)?;
    }
    Ok(r?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_sequences() {
        assert_eq!(parse_keys("Ctrl+4").unwrap(), vec![KeyStep::Combo(vec!["Ctrl"], "4".into())]);
        assert_eq!(
            parse_keys("Enter, wait 200, ctrl+shift+R").unwrap(),
            vec![
                KeyStep::Combo(vec![], "Enter".into()),
                KeyStep::Wait(200),
                KeyStep::Combo(vec!["Ctrl", "Shift"], "R".into())
            ]
        );
        assert!(parse_keys("").unwrap().is_empty());
        assert!(parse_keys("Ctrl+Banana").is_err());
        assert!(parse_keys("Hyper+A").is_err());
        assert_eq!(key_code("F12"), Some(0x7B));
    }
}
