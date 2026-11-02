//! Known folders, UI language, single instance and autostart.

use super::{from_wide_ptr, wide};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};

// Declared here instead of through windows-sys so it binds to ole32.dll
// (exported there on every Windows version, not only via combase.dll).
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(pv: *const c_void);
    fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
    fn CoUninitialize();
}

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

pub const WINDOW_TITLE: &str = "FeatherSend";
const MUTEX_NAME: &str = r"Local\FeatherSend.SingleInstance";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FeatherSend";

/// The user's Downloads folder (falls back to `%USERPROFILE%\Downloads`).
pub fn downloads() -> PathBuf {
    unsafe {
        let mut path: *mut u16 = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut path);
        if hr >= 0 && !path.is_null() {
            let result = PathBuf::from(from_wide_ptr(path));
            CoTaskMemFree(path as *const c_void);
            return result;
        }
        if !path.is_null() {
            CoTaskMemFree(path as *const c_void);
        }
    }
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("Downloads")
}

/// BCP 47 tag of the Windows display language (e.g. `it-IT`).
pub fn ui_languages() -> Vec<String> {
    unsafe {
        let mut buf = [0u16; 85];
        let len = LCIDToLocaleName(GetUserDefaultUILanguage() as u32, buf.as_mut_ptr(), 85, 0);
        if len > 1 {
            return vec![String::from_utf16_lossy(&buf[..len as usize - 1])];
        }
    }
    Vec::new()
}

pub fn open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    if !url.starts_with("https://") {
        return;
    }
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOW,
        );
    }
}

/// Opens a folder in Explorer, creating it first if it does not exist yet.
/// A dead network share can block this for a long time: not on the GUI thread.
pub fn open_folder(path: &Path) -> std::io::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    std::fs::create_dir_all(path)?;
    // ShellExecute can load shell extensions, which expect COM on the thread.
    let com = unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
    };
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(&path.to_string_lossy()).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOW,
        )
    } as isize;
    if com >= 0 {
        unsafe { CoUninitialize() };
    }
    // Documented: greater than 32 means success.
    if result > 32 { Ok(()) } else { Err(std::io::Error::other(format!("ShellExecute: {result}"))) }
}

pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "FeatherSend".into())
}

/// Held for the process lifetime; a second instance gets `None`.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn acquire_instance() -> Option<InstanceGuard> {
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, wide(MUTEX_NAME).as_ptr());
        if handle.is_null() {
            return None;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return None;
        }
        Some(InstanceGuard(handle))
    }
}

/// Brings the running instance's window to the front.
pub fn show_existing() {
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide(WINDOW_TITLE).as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

fn run_key(access: u32) -> Option<HKEY> {
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let status = RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut key);
        (status == 0).then_some(key)
    }
}

/// Adds or removes `"<exe>" --minimized` under HKCU\...\Run.
pub fn set_autostart(enabled: bool, exe: &Path) -> std::io::Result<()> {
    let key =
        run_key(KEY_SET_VALUE).ok_or_else(|| std::io::Error::other("Run key not available"))?;
    let status = unsafe {
        if enabled {
            let command = wide(&format!("\"{}\" --minimized", exe.display()));
            RegSetValueExW(
                key,
                wide(RUN_VALUE).as_ptr(),
                0,
                REG_SZ,
                command.as_ptr() as *const u8,
                (command.len() * 2) as u32,
            )
        } else {
            match RegDeleteValueW(key, wide(RUN_VALUE).as_ptr()) {
                2 => 0, // ERROR_FILE_NOT_FOUND: already off
                other => other,
            }
        }
    };
    unsafe { RegCloseKey(key) };
    if status == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(status as i32)) }
}

pub fn autostart_enabled() -> bool {
    let Some(key) = run_key(KEY_QUERY_VALUE) else { return false };
    let status = unsafe {
        RegQueryValueExW(
            key,
            wide(RUN_VALUE).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    unsafe { RegCloseKey(key) };
    status == 0
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    #[test]
    fn open_folder_reports_a_folder_it_cannot_create() {
        let file = std::env::temp_dir().join(format!("fs-open-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let result = open_folder(&file.join("sub"));
        std::fs::remove_file(&file).unwrap();
        assert!(result.is_err());
    }
}
