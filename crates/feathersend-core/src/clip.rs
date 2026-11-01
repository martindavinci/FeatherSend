//! Clipboard content as it travels between devices, the echo guard for
//! auto-sync, and DIB <-> PNG conversion (Windows clipboard bitmaps are DIBs,
//! the wire format is always PNG).

use crate::util::sha256;
use anyhow::{Context, bail, ensure};
use std::path::PathBuf;

pub const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;
pub const MIME_TEXT: &str = "text/plain; charset=utf-8";
pub const MIME_PNG: &str = "image/png";
/// Largest image accepted for the clipboard (about 100 MB as RGBA): a tiny
/// PNG can claim huge dimensions, so this is checked before decoding.
pub const MAX_PIXELS: u64 = 25_000_000;

/// The PNG header is valid and the image is not larger than [`MAX_PIXELS`].
pub fn png_within_limits(png_bytes: &[u8]) -> bool {
    png_dimensions(png_bytes)
        .is_some_and(|(w, h)| w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS)
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipContent {
    Text(String),
    Png(Vec<u8>),
    Files(Vec<PathBuf>),
}

impl ClipContent {
    /// Bytes compared by the echo guard (files are never auto-synced).
    pub fn fingerprint_bytes(&self) -> Option<&[u8]> {
        match self {
            ClipContent::Text(t) => Some(t.as_bytes()),
            ClipContent::Png(p) => Some(p),
            ClipContent::Files(_) => None,
        }
    }
}

/// Remembers what FeatherSend itself just wrote to the clipboard, so the
/// resulting change notification is not sent back to the network.
#[derive(Default)]
pub struct EchoGuard {
    last_written: Option<[u8; 32]>,
}

impl EchoGuard {
    pub fn note_written(&mut self, bytes: &[u8]) {
        self.last_written = Some(sha256(bytes));
    }

    /// True (once) when `bytes` is what was last written from the network.
    pub fn should_skip(&mut self, bytes: &[u8]) -> bool {
        if self.last_written == Some(sha256(bytes)) {
            self.last_written = None;
            return true;
        }
        false
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Converts a `CF_DIB`/`CF_DIBV5` payload (24 or 32 bpp, `BI_RGB` or the
/// standard `BI_BITFIELDS` masks, bottom-up or top-down) to PNG.
pub fn dib_to_png(dib: &[u8]) -> anyhow::Result<Vec<u8>> {
    let header = u32_at(dib, 0).context("short DIB")? as usize;
    ensure!(header >= 40, "unsupported DIB header size {header}");
    let width = u32_at(dib, 4).context("short DIB")? as i32;
    let height = u32_at(dib, 8).context("short DIB")? as i32;
    let bpp = u16_at(dib, 14).context("short DIB")?;
    let compression = u32_at(dib, 16).context("short DIB")?;
    let colors_used = u32_at(dib, 32).context("short DIB")? as usize;
    ensure!(width > 0 && height != 0, "empty bitmap");
    ensure!(
        u64::from(width.unsigned_abs()) * u64::from(height.unsigned_abs()) <= MAX_PIXELS,
        "bitmap too large"
    );
    ensure!(bpp == 24 || bpp == 32, "unsupported bit depth {bpp}");
    let mut offset = header + colors_used * 4;
    match compression {
        0 => {}
        3 => {
            // Right after a BITMAPINFOHEADER, or inside a V4/V5 header: same offset.
            let masks_at = 40;
            let masks = (
                u32_at(dib, masks_at).context("short DIB")?,
                u32_at(dib, masks_at + 4).context("short DIB")?,
                u32_at(dib, masks_at + 8).context("short DIB")?,
            );
            ensure!(masks == (0x00ff_0000, 0x0000_ff00, 0x0000_00ff), "unsupported masks");
            if header == 40 {
                offset += 12;
            }
        }
        other => bail!("unsupported DIB compression {other}"),
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let pixels = dib.get(offset..offset + stride * h).context("truncated DIB pixels")?;
    let step = bpp as usize / 8;
    let mut rgba = Vec::with_capacity(w * h * 4);
    let mut any_alpha = false;
    for y in 0..h {
        let row = if height > 0 { h - 1 - y } else { y };
        let line = &pixels[row * stride..row * stride + w * step];
        for px in line.chunks(step) {
            let a = if step == 4 { px[3] } else { 255 };
            any_alpha |= step == 4 && a != 0;
            rgba.extend_from_slice(&[px[2], px[1], px[0], a]);
        }
    }
    if step == 4 && !any_alpha {
        // Most apps leave the alpha byte at 0 in 32-bpp DIBs: treat as opaque.
        rgba.chunks_mut(4).for_each(|px| px[3] = 255);
    }
    encode_png(w as u32, h as u32, &rgba)
}

pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(out)
}

/// Decodes any PNG to 8-bit RGBA: `(width, height, pixels)`.
pub fn decode_png(png_bytes: &[u8]) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    ensure!(png_within_limits(png_bytes), "image too large or not a PNG");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().context("image too large")?];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => bail!("palette not expanded"),
    };
    Ok((w, h, rgba))
}

/// PNG to a 32-bpp bottom-up `CF_DIB` (BITMAPINFOHEADER + BGRA pixels).
pub fn png_to_dib(png_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let (w, h, rgba) = decode_png(png_bytes)?;
    let mut dib = Vec::with_capacity(40 + rgba.len());
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    dib.extend_from_slice(&[0u8; 16]); // resolution, colors used/important
    for row in rgba.chunks(w as usize * 4).rev() {
        for px in row.chunks(4) {
            dib.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Ok(dib)
}

/// Width and height from the PNG header, without decoding.
pub fn png_dimensions(png_bytes: &[u8]) -> Option<(u32, u32)> {
    if png_bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" || png_bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png_bytes.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png_bytes.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2, 24 bpp, bottom-up: rows padded to 8 bytes.
    fn dib24() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&2i32.to_le_bytes());
        d.extend_from_slice(&2i32.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&24u16.to_le_bytes());
        d.extend_from_slice(&[0u8; 24]);
        // bottom row: blue, green
        d.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        // top row: red, white
        d.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        d
    }

    #[test]
    fn echo_skipped_once() {
        let mut g = EchoGuard::default();
        g.note_written(b"hello");
        assert!(!g.should_skip(b"other"));
        assert!(g.should_skip(b"hello"));
        assert!(!g.should_skip(b"hello"));
    }

    #[test]
    fn dib24_bottom_up_roundtrip() {
        let png = dib_to_png(&dib24()).unwrap();
        assert_eq!(png_dimensions(&png), Some((2, 2)));
        let (_, _, rgba) = decode_png(&png).unwrap();
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "top-left red");
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255], "bottom-left blue");
        let back = dib_to_png(&png_to_dib(&png).unwrap()).unwrap();
        assert_eq!(decode_png(&back).unwrap().2, rgba);
    }

    #[test]
    fn dib32_top_down_zero_alpha_is_opaque() {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&1i32.to_le_bytes());
        d.extend_from_slice(&(-2i32).to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0u8; 24]);
        d.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);
        let (w, h, rgba) = decode_png(&dib_to_png(&d).unwrap()).unwrap();
        assert_eq!((w, h), (1, 2));
        assert_eq!(rgba, [255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn huge_claimed_dimensions_rejected_before_decoding() {
        let mut png = encode_png(1, 1, &[0, 0, 0, 255]).unwrap();
        png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(!png_within_limits(&png));
        assert!(decode_png(&png).is_err());
        assert!(png_to_dib(&png).is_err());
    }

    #[test]
    fn png_dimensions_rejects_garbage() {
        assert_eq!(png_dimensions(b"not a png"), None);
    }
}
