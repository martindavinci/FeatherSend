//! Win32 helpers for the main window. Moving controls goes through here, not
//! through nwg: with its `high-dpi` feature every `position`, `size` and
//! `set_*` call runs `GetDC(NULL)` without `ReleaseDC`, one leaked GDI handle
//! each, and a resize moves every control (10 000 handles is the limit).

use super::wide;
use native_windows_gui as nwg;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, COLOR_GRAYTEXT, CreateBitmap, CreateDIBSection,
    DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_WORDBREAK, DeleteObject, DrawTextW, GetDC,
    GetDeviceCaps, GetSysColor, HDC, HGDIOBJ, LOGPIXELSX, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::Controls::{
    CDDS_POSTPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYPOSTPAINT, ILC_COLOR32, ILC_MASK,
    ImageList_Create, ImageList_ReplaceIcon, LVHITTESTINFO, LVM_GETHEADER,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SUBITEMHITTEST, LVS_EX_DOUBLEBUFFER, NMCUSTOMDRAW, NMHDR,
    NMMOUSE, SB_SETPARTS, SetWindowTheme,
};
use windows_sys::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, HDROP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CreateIconIndirect, DeferWindowPos, DestroyIcon, EndDeferWindowPos,
    GetClientRect, GetSystemMetrics, GetWindowRect, HICON, ICONINFO, SM_CXSMICON, SWP_NOACTIVATE,
    SWP_NOZORDER, SendMessageW, WM_GETFONT,
};

/// Screen DPI / 96. The manifest declares system DPI awareness, so it is
/// fixed for the process lifetime.
pub fn scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
        ReleaseDC(std::ptr::null_mut(), dc);
        if dpi > 0 { f64::from(dpi) / 96.0 } else { 1.0 }
    })
}

/// Logical (96 DPI) units to pixels.
pub fn px(logical: i32) -> i32 {
    (f64::from(logical) * scale()).round() as i32
}

/// Pixels to logical units.
pub fn logical(pixels: i32) -> i32 {
    (f64::from(pixels) / scale()).round() as i32
}

/// Client area size in pixels.
pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetClientRect(hwnd, &mut rect) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Moves and sizes child windows in one go (`[x, y, w, h]` in pixels): one
/// repaint, no flicker while dragging the window edge.
pub fn place(moves: &[(HWND, [i32; 4])]) {
    unsafe {
        let mut batch = BeginDeferWindowPos(moves.len() as i32);
        for (hwnd, [x, y, w, h]) in moves {
            if batch.is_null() {
                return;
            }
            batch = DeferWindowPos(
                batch,
                *hwnd,
                std::ptr::null_mut(),
                *x,
                *y,
                *w,
                *h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if !batch.is_null() {
            EndDeferWindowPos(batch);
        }
    }
}

/// The modern list look (hover, selection) on Windows 7 and later, and the
/// control's own double buffering, which needs no permanent bitmaps (nwg's
/// `double_buffer` keeps one per list, about 600 KB each).
pub fn modern_list(hwnd: HWND) {
    unsafe {
        SetWindowTheme(hwnd, wide("Explorer").as_ptr(), std::ptr::null());
        SendMessageW(
            hwnd,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            LVS_EX_DOUBLEBUFFER as WPARAM,
            LVS_EX_DOUBLEBUFFER as LPARAM,
        );
    }
}

/// Makes a child window accept dropped files itself: `WM_DROPFILES` then goes
/// to it, with the drop point in its own coordinates.
pub fn accept_files(hwnd: HWND) {
    unsafe { DragAcceptFiles(hwnd, 1) };
}

/// The dropped paths and point of a `WM_DROPFILES`; finishes the drop (nwg's
/// own `OnFileDrop` never calls `DragFinish`, which leaks the block).
///
/// # Safety
/// `wparam` must be the `WPARAM` of a `WM_DROPFILES` message, used once.
pub unsafe fn take_drop(wparam: WPARAM) -> (Vec<PathBuf>, POINT) {
    let drop = wparam as HDROP;
    let mut point = POINT { x: 0, y: 0 };
    unsafe {
        DragQueryPoint(drop, &mut point);
        let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
            paths.push(PathBuf::from(OsString::from_wide(&buf[..len])));
        }
        DragFinish(drop);
        (paths, point)
    }
}

/// The row of a list view under a point in its client coordinates.
pub fn list_row_at(list: HWND, point: POINT) -> Option<usize> {
    let mut hit = LVHITTESTINFO { pt: point, flags: 0, iItem: -1, iSubItem: 0, iGroup: 0 };
    let row = unsafe { SendMessageW(list, LVM_SUBITEMHITTEST, 0, &mut hit as *mut _ as LPARAM) };
    usize::try_from(row).ok()
}

/// By how much 16-pixel icons are enlarged: small icons are 32 pixels at 200 %.
pub fn icon_factor() -> usize {
    (unsafe { GetSystemMetrics(SM_CXSMICON) } / 16).max(1) as usize
}

/// An icon this program made; destroyed on drop.
pub struct OwnedIcon(HICON);

impl OwnedIcon {
    pub fn handle(&self) -> HICON {
        self.0
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.0) };
    }
}

/// Builds an icon from `size` × `size` ARGB pixels (top row first).
pub fn create_icon(argb: &[u32], size: usize) -> Option<OwnedIcon> {
    assert_eq!(argb.len(), size * size);
    let side = size as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    // The AND mask: one bit per pixel, 1 = transparent, rows of whole words.
    let stride = size.div_ceil(16) * 2;
    let mut mask = vec![0u8; stride * size];
    for (i, pixel) in argb.iter().enumerate() {
        if pixel >> 24 == 0 {
            mask[(i / size) * stride + (i % size) / 8] |= 0x80 >> ((i % size) % 8);
        }
    }
    unsafe {
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(argb.as_ptr(), bits as *mut u32, argb.len());
        let mask = CreateBitmap(side, side, 1, 1, mask.as_ptr() as *const c_void);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        // The icon keeps its own copies.
        DeleteObject(color as HGDIOBJ);
        DeleteObject(mask as HGDIOBJ);
        (!icon.is_null()).then_some(OwnedIcon(icon))
    }
}

/// An image list of `size`-pixel icons, for a list view's small images. The
/// list view destroys it with itself (it is created without
/// `LVS_SHAREIMAGELISTS`), hence `owned: false`.
pub fn image_list(icons: &[Vec<u32>], size: usize) -> nwg::ImageList {
    let side = size as i32;
    unsafe {
        let list = ImageList_Create(side, side, ILC_COLOR32 | ILC_MASK, icons.len() as i32, 0);
        for pixels in icons {
            if let Some(icon) = create_icon(pixels, size) {
                ImageList_ReplaceIcon(list, -1, icon.handle());
            }
        }
        nwg::ImageList { handle: list as _, owned: false }
    }
}

/// The outer height of a window in pixels.
pub fn window_height(hwnd: HWND) -> i32 {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(hwnd, &mut rect) };
    rect.bottom - rect.top
}

/// Splits a status bar: `edges` are the parts' right edges in pixels, -1 for
/// "up to the window edge".
pub fn set_status_parts(status: HWND, edges: &[i32]) {
    unsafe { SendMessageW(status, SB_SETPARTS, edges.len(), edges.as_ptr() as LPARAM) };
}

/// Sender and code of a `WM_NOTIFY`.
///
/// # Safety
/// `lparam` must be the `LPARAM` of a `WM_NOTIFY` message.
pub unsafe fn notify_header(lparam: LPARAM) -> (HWND, u32) {
    let header = unsafe { &*(lparam as *const NMHDR) };
    (header.hwndFrom, header.code)
}

/// Answers an empty list view's `NM_CUSTOMDRAW`: asks for the post-paint
/// stage, then draws `text` centered in gray below the header. (The control's
/// own `LVN_GETEMPTYMARKUP` never arrives in this window: seen on Windows 10.)
///
/// # Safety
/// `lparam` must be the `LPARAM` of a list view's `NM_CUSTOMDRAW` notification.
pub unsafe fn draw_empty_hint(lparam: LPARAM, text: &str) -> LRESULT {
    let draw = unsafe { &*(lparam as *const NMCUSTOMDRAW) };
    match draw.dwDrawStage {
        CDDS_PREPAINT => CDRF_NOTIFYPOSTPAINT as LRESULT,
        CDDS_POSTPAINT => {
            unsafe { draw_hint(draw.hdr.hwndFrom, draw.hdc, text) };
            CDRF_DODEFAULT as LRESULT
        }
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

unsafe fn draw_hint(list: HWND, dc: HDC, text: &str) {
    let (width, height) = client_size(list);
    unsafe {
        let header = SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND;
        let mut rect = RECT {
            left: px(16),
            top: window_height(header) + px(18),
            right: width - px(16),
            bottom: height,
        };
        let font = SendMessageW(list, WM_GETFONT, 0, 0) as HGDIOBJ;
        let old_font = SelectObject(dc, font);
        let old_color = SetTextColor(dc, GetSysColor(COLOR_GRAYTEXT));
        let old_mode = SetBkMode(dc, TRANSPARENT as i32);
        let units: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            dc,
            units.as_ptr(),
            units.len() as i32,
            &mut rect,
            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
        SetBkMode(dc, old_mode);
        SetTextColor(dc, old_color);
        SelectObject(dc, old_font);
    }
}

/// The part a status bar `NM_CLICK` hit.
///
/// # Safety
/// `lparam` must be the `LPARAM` of a status bar `NM_CLICK` notification.
pub unsafe fn clicked_part(lparam: LPARAM) -> usize {
    unsafe { &*(lparam as *const NMMOUSE) }.dwItemSpec
}
