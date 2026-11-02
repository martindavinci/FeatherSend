//! Where every control of the main window goes, in logical (96 DPI) units.
//! Pure arithmetic, so the rules are unit tested:
//!
//! - the two lists share the left column, devices on top (55 %);
//! - beside the devices: the send group (primary button first and taller),
//!   then the device group (pair, add by IP);
//! - beside the transfers: cancel and resume at the top, the app group
//!   (received folder, settings, about) at the bottom.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w: w.max(0), h: h.max(0) }
    }
}

/// The smallest window (outer size) the layout is designed for.
pub const MIN_WINDOW: (i32, i32) = (760, 540);
/// The client area of a new window.
pub const DEFAULT_CLIENT: (u32, u32) = (820, 560);

const MARGIN: i32 = 10;
const LABEL_H: i32 = 18;
const LABEL_GAP: i32 = 4;
const SIDE_W: i32 = 180;
const BUTTON_H: i32 = 28;
const PRIMARY_H: i32 = 38;
const BUTTON_GAP: i32 = 6;
const GROUP_GAP: i32 = 18;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MainLayout {
    pub devices_label: Rect,
    pub devices: Rect,
    pub transfers_label: Rect,
    pub transfers: Rect,
    pub send_files: Rect,
    pub send_folder: Rect,
    pub send_clipboard: Rect,
    pub pair: Rect,
    pub add_ip: Rect,
    pub cancel: Rect,
    pub resume: Rect,
    pub open_folder: Rect,
    pub settings: Rect,
    pub about: Rect,
}

/// Stacks buttons downwards from `y`; returns the rects and the next free y.
fn stack(x: i32, mut y: i32, heights: &[i32]) -> (Vec<Rect>, i32) {
    let mut rects = Vec::with_capacity(heights.len());
    for h in heights {
        rects.push(Rect::new(x, y, SIDE_W, *h));
        y += h + BUTTON_GAP;
    }
    (rects, y - BUTTON_GAP)
}

/// The layout for a client area of `w` × `h` (status bar excluded).
pub fn main_layout(w: i32, h: i32) -> MainLayout {
    let side_x = w - MARGIN - SIDE_W;
    let list_w = side_x - 2 * MARGIN;
    let header = LABEL_H + LABEL_GAP;
    let devices_y = MARGIN + header;
    let lists_h = h - 2 * MARGIN - 2 * header - MARGIN;
    let devices_h = lists_h * 55 / 100;
    let transfers_label_y = devices_y + devices_h + MARGIN;
    let transfers_y = transfers_label_y + header;
    let transfers_h = h - MARGIN - transfers_y;

    let (send, next) = stack(side_x, devices_y, &[PRIMARY_H, BUTTON_H, BUTTON_H]);
    let (device, _) = stack(side_x, next + GROUP_GAP, &[BUTTON_H, BUTTON_H]);
    let (transfer, _) = stack(side_x, transfers_y, &[BUTTON_H, BUTTON_H]);
    let app_h = 3 * BUTTON_H + 2 * BUTTON_GAP;
    let (app, _) =
        stack(side_x, transfers_y + transfers_h - app_h, &[BUTTON_H, BUTTON_H, BUTTON_H]);

    MainLayout {
        devices_label: Rect::new(MARGIN, MARGIN, list_w, LABEL_H),
        devices: Rect::new(MARGIN, devices_y, list_w, devices_h),
        transfers_label: Rect::new(MARGIN, transfers_label_y, list_w, LABEL_H),
        transfers: Rect::new(MARGIN, transfers_y, list_w, transfers_h),
        send_files: send[0],
        send_folder: send[1],
        send_clipboard: send[2],
        pair: device[0],
        add_ip: device[1],
        cancel: transfer[0],
        resume: transfer[1],
        open_folder: app[0],
        settings: app[1],
        about: app[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Rect {
        fn bottom(&self) -> i32 {
            self.y + self.h
        }

        fn right(&self) -> i32 {
            self.x + self.w
        }

        fn overlaps(&self, other: &Rect) -> bool {
            self.x < other.right()
                && other.x < self.right()
                && self.y < other.bottom()
                && other.y < self.bottom()
        }
    }

    impl MainLayout {
        /// Every button, top to bottom.
        fn buttons(&self) -> [Rect; 10] {
            [
                self.send_files,
                self.send_folder,
                self.send_clipboard,
                self.pair,
                self.add_ip,
                self.cancel,
                self.resume,
                self.open_folder,
                self.settings,
                self.about,
            ]
        }
    }

    /// Client area of the smallest window: minus the frame and caption
    /// (Windows 10 at 96 DPI) and the status bar.
    const MIN_CLIENT: (i32, i32) = (MIN_WINDOW.0 - 16, MIN_WINDOW.1 - 39 - 23);

    fn sizes() -> [(i32, i32); 3] {
        [MIN_CLIENT, (DEFAULT_CLIENT.0 as i32, DEFAULT_CLIENT.1 as i32 - 23), (1600, 1000)]
    }

    #[test]
    fn buttons_never_overlap() {
        for (w, h) in sizes() {
            let l = main_layout(w, h);
            let buttons = l.buttons();
            for (i, a) in buttons.iter().enumerate() {
                for b in &buttons[i + 1..] {
                    assert!(!a.overlaps(b), "{w}x{h}: {a:?} overlaps {b:?}");
                }
                assert!(!a.overlaps(&l.devices) && !a.overlaps(&l.transfers));
            }
        }
    }

    #[test]
    fn everything_fits_in_the_client_area() {
        for (w, h) in sizes() {
            let l = main_layout(w, h);
            for r in l.buttons().iter().chain([&l.devices, &l.transfers]) {
                assert!(r.x >= 0 && r.y >= 0 && r.right() <= w && r.bottom() <= h, "{r:?}");
            }
        }
    }

    #[test]
    fn device_buttons_stay_beside_the_device_list() {
        for (w, h) in sizes() {
            let l = main_layout(w, h);
            assert!(l.add_ip.bottom() <= l.devices.bottom(), "{w}x{h}");
            assert!(l.cancel.y >= l.transfers.y);
        }
    }

    #[test]
    fn primary_button_is_first_and_taller() {
        let l = main_layout(MIN_CLIENT.0, MIN_CLIENT.1);
        assert!(l.send_files.h > l.send_folder.h);
        assert_eq!(l.send_files.y, l.devices.y);
        assert!(l.pair.y - l.send_clipboard.bottom() > BUTTON_GAP, "groups are apart");
    }

    #[test]
    fn app_group_sits_at_the_bottom() {
        for (w, h) in sizes() {
            let l = main_layout(w, h);
            assert_eq!(l.about.bottom(), l.transfers.bottom());
            assert!(l.open_folder.y > l.resume.bottom());
        }
    }

    #[test]
    fn lists_share_the_left_column() {
        let l = main_layout(1000, 700);
        assert_eq!((l.devices.x, l.devices.w), (l.transfers.x, l.transfers.w));
        assert_eq!(l.devices.right() + MARGIN, l.send_files.x);
        assert!(l.devices.h > l.transfers.h);
    }
}
