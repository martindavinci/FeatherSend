//! Popup menus built on the fly (device list, tray, hotkey target choice).

use super::wide;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, PostMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_NULL,
};

pub enum Item {
    Entry { id: u32, text: String, checked: bool, enabled: bool },
    Separator,
}

impl Item {
    pub fn new(id: u32, text: &str) -> Item {
        Item::Entry { id, text: text.to_string(), checked: false, enabled: true }
    }

    pub fn check(id: u32, text: &str, checked: bool, enabled: bool) -> Item {
        Item::Entry { id, text: text.to_string(), checked, enabled }
    }
}

/// Shows the menu at the cursor and returns the chosen id (0 = none).
pub fn popup(hwnd: HWND, items: &[Item]) -> u32 {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return 0;
        }
        for item in items {
            match item {
                Item::Entry { id, text, checked, enabled } => {
                    let mut flags = MF_STRING;
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    AppendMenuW(menu, flags, *id as usize, wide(text).as_ptr());
                }
                Item::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                }
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required for tray menus to close when clicking elsewhere.
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen as u32
    }
}
