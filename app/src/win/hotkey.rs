//! The global clipboard hotkey (`RegisterHotKey`), written like `Ctrl+Alt+C`.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};

/// `(modifiers, virtual key)`; at least one modifier is required.
pub fn parse(text: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            k if key.is_none() => key = Some(virtual_key(k)?),
            _ => return None,
        }
    }
    (mods != 0).then_some(())?;
    Some((mods, key?))
}

fn virtual_key(name: &str) -> Option<u32> {
    let bytes = name.as_bytes();
    match bytes {
        [c] if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as u32),
        [b'f', rest @ ..] if !rest.is_empty() => {
            let n: u32 = std::str::from_utf8(rest).ok()?.parse().ok()?;
            (1..=24).contains(&n).then_some(0x70 + n - 1)
        }
        _ => None,
    }
}

pub fn register(hwnd: HWND, id: i32, text: &str) -> bool {
    let Some((mods, vk)) = parse(text) else { return false };
    unsafe { RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk) != 0 }
}

pub fn unregister(hwnd: HWND, id: i32) {
    unsafe { UnregisterHotKey(hwnd, id) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_parse_default() {
        assert_eq!(parse("Ctrl+Alt+C"), Some((MOD_CONTROL | MOD_ALT, 0x43)));
        assert_eq!(parse("shift + win + f5"), Some((MOD_SHIFT | MOD_WIN, 0x74)));
    }

    #[test]
    fn hotkey_parse_rejects_garbage() {
        assert_eq!(parse("C"), None, "a modifier is required");
        assert_eq!(parse("Ctrl+Alt"), None);
        assert_eq!(parse("Ctrl+Alt+C+D"), None);
        assert_eq!(parse("Ctrl+F25"), None);
        assert_eq!(parse("Ctrl+Space"), None);
    }
}
