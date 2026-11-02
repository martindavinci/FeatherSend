//! The small icons, drawn in code as 16 × 16 pixel maps and turned into
//! bitmaps at startup: no resource files, no image decoder, and only a few
//! KB of bitmaps. Pure data and arithmetic, so it is unit tested; `win::ui`
//! makes the Windows icons.

use feathersend_core::backend::{DeviceClass, DeviceKind, DeviceView};

pub const SIZE: usize = 16;

type Map = [&'static str; SIZE];

const PHONE: Map = [
    "................",
    ".....KKKKKK.....",
    "....KBBBBBBK....",
    "....KSSSSSSK....",
    "....KSLLSSSK....",
    "....KSLSSSSK....",
    "....KSSSSSSK....",
    "....KSSSSSSK....",
    "....KSSSSSSK....",
    "....KSSSSSSK....",
    "....KSSSSSSK....",
    "....KSSSSSSK....",
    "....KBBBBBBK....",
    "....KBBDDBBK....",
    ".....KKKKKK.....",
    "................",
];

const COMPUTER: Map = [
    "................",
    "................",
    ".KKKKKKKKKKKKKK.",
    ".KSSSSSSSSSSSSK.",
    ".KSLLLSSSSSSSSK.",
    ".KSLSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KKKKKKKKKKKKKK.",
    "......GGGG......",
    "......GGGG......",
    "...DDDDDDDDDD...",
    "................",
];

/// A green check, drawn over the bottom-right corner of paired devices.
const PAIRED_BADGE: Map = [
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "...........ggg..",
    "..........ggggW.",
    ".........ggggWgg",
    ".........gWgWggg",
    ".........ggWgggg",
    "..........ggggg.",
    "...........ggg..",
];

/// The primary button's arrow.
const SEND: Map = [
    "................",
    "................",
    ".........KK.....",
    ".........KSK....",
    ".........KSSK...",
    ".KKKKKKKKKSSSK..",
    ".KSSSSSSSSSSSSK.",
    ".KSSSSSSSSSSSSK.",
    ".KKKKKKKKKSSSK..",
    ".........KSSK...",
    ".........KSK....",
    ".........KK.....",
    "................",
    "................",
    "................",
    "................",
];

/// ARGB of a map character; transparent is 0. No pure black: fully opaque
/// colors stay visible even if a drawing path treats black as the mask.
fn color(c: char) -> u32 {
    match c {
        'K' => 0xFF2F_3B4C,
        'B' => 0xFFD5_DCE4,
        'S' => 0xFF3F_8EDC,
        'L' => 0xFF9C_CBF5,
        'G' => 0xFF8A_97A8,
        'D' => 0xFF56_626F,
        'g' => 0xFF2E_9E4F,
        'W' => 0xFFFF_FFFF,
        _ => 0,
    }
}

fn pixels(map: &Map) -> Vec<u32> {
    map.iter().flat_map(|row| row.chars().map(color)).collect()
}

/// Gray and lighter, for offline devices. Alpha stays as it is.
fn dimmed(argb: u32) -> u32 {
    let [b, g, r, a] = argb.to_le_bytes();
    let gray = (u32::from(r) * 30 + u32::from(g) * 59 + u32::from(b) * 11) / 100;
    let light = (gray + 0xC8) / 2;
    u32::from_le_bytes([light as u8, light as u8, light as u8, a])
}

/// `online` in full color, offline dimmed; a paired device gets the badge.
fn device_pixels(class: DeviceClass, paired: bool, online: bool) -> Vec<u32> {
    let mut out = pixels(if class == DeviceClass::Mobile { &PHONE } else { &COMPUTER });
    if paired {
        for (pixel, badge) in out.iter_mut().zip(pixels(&PAIRED_BADGE)) {
            if badge != 0 {
                *pixel = badge;
            }
        }
    }
    if !online {
        out.iter_mut().for_each(|p| *p = dimmed(*p));
    }
    out
}

/// Image list row of a device, matching [`device_icons`].
pub fn device_icon_index(d: &DeviceView) -> i32 {
    let mobile = i32::from(d.class == DeviceClass::Mobile);
    let paired = i32::from(d.kind == DeviceKind::Paired);
    mobile * 4 + paired * 2 + i32::from(!d.online)
}

/// The pixels of every device icon, in image list order.
pub fn device_icons() -> Vec<Vec<u32>> {
    let mut icons = Vec::with_capacity(8);
    for class in [DeviceClass::Desktop, DeviceClass::Mobile] {
        for paired in [false, true] {
            for online in [true, false] {
                icons.push(device_pixels(class, paired, online));
            }
        }
    }
    icons
}

pub fn send_icon() -> Vec<u32> {
    pixels(&SEND)
}

/// Doubles (triples…) every pixel, for high DPI: pixel art stays crisp.
pub fn scaled(pixels: &[u32], factor: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(pixels.len() * factor * factor);
    for row in pixels.chunks(SIZE) {
        for _ in 0..factor {
            for pixel in row {
                out.extend(std::iter::repeat_n(*pixel, factor));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(class: DeviceClass, kind: DeviceKind, online: bool) -> DeviceView {
        DeviceView {
            fingerprint: "F".into(),
            alias: "a".into(),
            address: None,
            class,
            kind,
            feathersend: false,
            online,
            auto_accept: false,
            auto_sync: false,
        }
    }

    #[test]
    fn maps_are_square_and_use_known_colors() {
        for map in [&PHONE, &COMPUTER, &PAIRED_BADGE, &SEND] {
            for row in map {
                assert_eq!(row.chars().count(), SIZE, "{row}");
                for c in row.chars() {
                    assert!(c == '.' || color(c) != 0, "unknown color {c:?} in {row}");
                }
            }
        }
    }

    #[test]
    fn no_color_is_pure_black_and_all_are_opaque() {
        for c in "KBSLGDgW".chars() {
            assert_eq!(color(c) >> 24, 0xFF);
            assert_ne!(color(c) & 0x00FF_FFFF, 0);
        }
    }

    #[test]
    fn index_matches_the_image_list_order() {
        let icons = device_icons();
        assert_eq!(icons.len(), 8);
        for class in [DeviceClass::Desktop, DeviceClass::Mobile] {
            for (kind, paired) in [(DeviceKind::LocalSend, false), (DeviceKind::Paired, true)] {
                for online in [true, false] {
                    let i = device_icon_index(&view(class, kind, online)) as usize;
                    assert_eq!(icons[i], device_pixels(class, paired, online));
                }
            }
        }
    }

    #[test]
    fn other_devices_look_like_computers() {
        assert_eq!(
            device_icon_index(&view(DeviceClass::Other, DeviceKind::FeatherSend, true)),
            device_icon_index(&view(DeviceClass::Desktop, DeviceKind::LocalSend, true)),
        );
    }

    #[test]
    fn variants_differ_visibly() {
        let plain = device_pixels(DeviceClass::Mobile, false, true);
        assert_ne!(plain, device_pixels(DeviceClass::Mobile, true, true), "paired badge");
        assert_ne!(plain, device_pixels(DeviceClass::Desktop, false, true), "phone vs computer");
        let offline = device_pixels(DeviceClass::Mobile, false, false);
        assert!(offline.iter().all(|p| {
            let [b, g, r, _] = p.to_le_bytes();
            b == g && g == r
        }));
        assert_eq!(
            plain.iter().map(|p| p >> 24).collect::<Vec<_>>(),
            offline.iter().map(|p| p >> 24).collect::<Vec<_>>(),
            "the same shape"
        );
    }

    #[test]
    fn scaling_keeps_the_picture() {
        let icon = send_icon();
        let double = scaled(&icon, 2);
        assert_eq!(double.len(), 4 * icon.len());
        for y in 0..SIZE {
            for x in 0..SIZE {
                let at = |dx: usize, dy: usize| double[(2 * y + dy) * 2 * SIZE + 2 * x + dx];
                let expected = icon[y * SIZE + x];
                assert_eq!([at(0, 0), at(1, 0), at(0, 1), at(1, 1)], [expected; 4]);
            }
        }
        assert_eq!(scaled(&icon, 1), icon);
    }
}
