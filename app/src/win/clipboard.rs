//! Reading and writing the Windows clipboard: text, images (PNG and DIB)
//! and files (`CF_HDROP`).

use super::{from_wide_ptr, wide};
use feathersend_core::clip::{ClipContent, MAX_CLIPBOARD_BYTES, dib_to_png, png_to_dib};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

struct Open;

impl Open {
    /// Other programs hold the clipboard briefly: retry a few times.
    fn new(hwnd: HWND) -> Option<Open> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(hwnd) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(wide("PNG").as_ptr()) }
}

/// Copies the bytes of a clipboard handle (`None` above the size limit).
unsafe fn global_bytes(handle: HANDLE, limit: usize) -> Option<Vec<u8>> {
    unsafe {
        let size = GlobalSize(handle);
        if size == 0 || size > limit {
            return None;
        }
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }
}

/// The current clipboard content, preferring files, then images, then text.
pub fn read(hwnd: HWND) -> Option<ClipContent> {
    let _open = Open::new(hwnd)?;
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP as u32) != 0 {
            let drop = GetClipboardData(CF_HDROP as u32) as HDROP;
            if !drop.is_null() {
                let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
                let mut files = Vec::new();
                for i in 0..count {
                    let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0) as usize;
                    let mut buf = vec![0u16; len + 1];
                    DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
                    files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
                }
                if !files.is_empty() {
                    return Some(ClipContent::Files(files));
                }
            }
        }
        // Images may be large; DIBs are converted, so allow some headroom.
        let image_limit = MAX_CLIPBOARD_BYTES * 8;
        let png = png_format();
        if png != 0
            && IsClipboardFormatAvailable(png) != 0
            && let Some(bytes) = global_bytes(GetClipboardData(png), image_limit)
        {
            return Some(ClipContent::Png(bytes));
        }
        for format in [CF_DIBV5, CF_DIB] {
            if IsClipboardFormatAvailable(format as u32) != 0
                && let Some(dib) = global_bytes(GetClipboardData(format as u32), image_limit)
                && let Ok(png) = dib_to_png(&dib)
            {
                return Some(ClipContent::Png(png));
            }
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) != 0 {
            let handle = GetClipboardData(CF_UNICODETEXT as u32);
            let ptr = GlobalLock(handle) as *const u16;
            if !ptr.is_null() {
                let text = from_wide_ptr(ptr);
                GlobalUnlock(handle);
                if !text.is_empty() {
                    return Some(ClipContent::Text(text));
                }
            }
        }
    }
    None
}

/// Moves `bytes` into a movable global block for `SetClipboardData`.
unsafe fn set_bytes(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if handle.is_null() {
            return false;
        }
        let ptr = GlobalLock(handle) as *mut u8;
        if ptr.is_null() {
            GlobalFree(handle);
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        GlobalUnlock(handle);
        if SetClipboardData(format, handle).is_null() {
            GlobalFree(handle);
            return false;
        }
        true
    }
}

fn utf16_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

pub fn write(hwnd: HWND, content: &ClipContent) -> anyhow::Result<()> {
    let _open = Open::new(hwnd).ok_or_else(|| anyhow::anyhow!("clipboard busy"))?;
    unsafe {
        EmptyClipboard();
        let ok = match content {
            ClipContent::Text(text) => {
                // Windows expects CRLF line ends; other systems send LF.
                let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
                set_bytes(CF_UNICODETEXT as u32, &utf16_bytes(&wide(&crlf)))
            }
            ClipContent::Png(png) => {
                let dib_ok =
                    png_to_dib(png).map(|dib| set_bytes(CF_DIB as u32, &dib)).unwrap_or(false);
                set_bytes(png_format(), png) || dib_ok
            }
            ClipContent::Files(paths) => {
                let mut list: Vec<u16> = Vec::new();
                for path in paths {
                    list.extend(path.as_os_str().to_string_lossy().encode_utf16());
                    list.push(0);
                }
                list.push(0);
                let header = DROPFILES {
                    pFiles: std::mem::size_of::<DROPFILES>() as u32,
                    pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
                    fNC: 0,
                    fWide: 1,
                };
                let mut bytes = std::slice::from_raw_parts(
                    &header as *const DROPFILES as *const u8,
                    std::mem::size_of::<DROPFILES>(),
                )
                .to_vec();
                bytes.extend(utf16_bytes(&list));
                set_bytes(CF_HDROP as u32, &bytes)
            }
        };
        anyhow::ensure!(ok, "cannot set the clipboard");
    }
    Ok(())
}

pub fn add_listener(hwnd: HWND) -> bool {
    unsafe { AddClipboardFormatListener(hwnd) != 0 }
}

pub fn remove_listener(hwnd: HWND) {
    unsafe { RemoveClipboardFormatListener(hwnd) };
}

pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}
