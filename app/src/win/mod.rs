//! Thin Win32 wrappers (windows-sys) for what native-windows-gui lacks.

pub mod clipboard;
pub mod hotkey;
pub mod menu;
pub mod netwatch;
pub mod system;
pub mod ui;

use windows_sys::Win32::Foundation::HWND;

/// NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `ptr` must point to a NUL-terminated UTF-16 buffer.
pub unsafe fn from_wide_ptr(ptr: *const u16) -> String {
    let mut len = 0;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

/// The raw window handle of an nwg control, for windows-sys calls.
pub fn hwnd_of(handle: &native_windows_gui::ControlHandle) -> HWND {
    handle.hwnd().map(|h| h as HWND).unwrap_or(std::ptr::null_mut())
}
