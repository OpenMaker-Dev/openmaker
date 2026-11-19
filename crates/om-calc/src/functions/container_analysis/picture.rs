//! Picture headers: size, resolution, transparency and photo metadata of
//! PNG, JPEG, GIF, BMP, TIFF and WebP data, read from the bytes whatever
//! the file name says (a PNG named `a.jpg` is 3 x 2 all the same).
//!
//! Resolutions are what FileMaker Server's image library reports, in its
//! own units: PNG's dots per meter divided by 100 (11811 is 118 "dpi"),
//! JFIF and EXIF values as stored whatever their unit, BMP's dots per meter
//! divided by 100. A JFIF density of 1:1 with no unit (an aspect ratio) and
//! zero values are no resolution; EXIF wins over JFIF.

use super::exif::{self, Exif};

/// What the picture's header says.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ImageInfo {
    /// Pixel size as stored, before orientation.
    pub width: u32,
    pub height: u32,
    pub resolution: Option<(f64, f64)>,
    /// An alpha channel or a transparent color (PNG `tRNS`, a GIF
    /// transparent index).
    pub alpha: bool,
    /// EXIF metadata (JPEG, WebP) or the TIFF file's own tags.
    pub exif: Option<Exif>,
}

impl ImageInfo {
    /// The EXIF orientation tag, when there is one.
    pub fn orientation(&self) -> Option<u16> {
        self.exif.as_ref().and_then(|e| e.orientation)
    }

    /// Orientations 5 to 8 turn the picture a quarter.
    pub fn quarter_turn(&self) -> bool {
        matches!(self.orientation(), Some(5..=8))
    }

    /// The size as displayed, after orientation.
    pub fn display_size(&self) -> (u32, u32) {
        if self.quarter_turn() {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// The resolution as `GetContainerAttribute` shows it: rounded, 72 when
    /// the picture has none.
    pub fn dpi(&self) -> (u32, u32) {
        match self.resolution {
            Some((x, y)) => (round(x), round(y)),
            None => (72, 72),
        }
    }
}

fn round(x: f64) -> u32 {
    (x + 0.5).floor().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// The picture formats the header readers know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tiff,
    Webp,
}

/// The format of `data` from its signature.
pub(super) fn sniff(data: &[u8]) -> Option<Format> {
    Some(match data {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Format::Png,
        [0xff, 0xd8, 0xff, ..] => Format::Jpeg,
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Format::Gif,
        [b'B', b'M', ..] => Format::Bmp,
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Format::Tiff,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Format::Webp,
        _ => return None,
    })
}

/// Reads the header of picture data; `None` when it is not a picture the
/// readers know or its header is damaged.
pub(super) fn read(data: &[u8]) -> Option<ImageInfo> {
    let info = match sniff(data)? {
        Format::Png => png(data),
        Format::Jpeg => jpeg(data),
        Format::Gif => gif(data),
        Format::Bmp => bmp(data),
        Format::Tiff => tiff(data),
        Format::Webp => webp(data),
    }?;
    (info.width > 0 && info.height > 0).then_some(info)
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn le16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn le24(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// A resolution pair when both values are positive.
fn positive(x: f64, y: f64) -> Option<(f64, f64)> {
    (x > 0.0 && y > 0.0).then_some((x, y))
}

fn png(d: &[u8]) -> Option<ImageInfo> {
    if d.get(12..16)? != b"IHDR" {
        return None;
    }
    let mut info = ImageInfo {
        width: be32(d, 16)?,
        height: be32(d, 20)?,
        alpha: matches!(d.get(25)?, 4 | 6),
        ..ImageInfo::default()
    };
    let mut at = 8;
    while let (Some(len), Some(kind)) = (be32(d, at), d.get(at + 4..at + 8)) {
        let body = at + 8;
        match kind {
            b"pHYs" => {
                let (x, y) = (f64::from(be32(d, body)?), f64::from(be32(d, body + 4)?));
                info.resolution = match d.get(body + 8)? {
                    1 => positive(x / 100.0, y / 100.0),
                    _ => positive(x, y),
                };
            }
            b"tRNS" => info.alpha = true,
            b"IEND" => break,
            _ => {}
        }
        at = body
            .checked_add(usize::try_from(len).ok()?)?
            .checked_add(4)?;
    }
    Some(info)
}

fn jpeg(d: &[u8]) -> Option<ImageInfo> {
    let mut info = ImageInfo::default();
    let mut jfif = None;
    let mut at = 2;
    // Segments up to the scan; whatever follows a damaged one is unknown.
    loop {
        // Markers may be padded with fill bytes.
        while d.get(at) == Some(&0xff) && d.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        let (Some(0xff), Some(&marker)) = (d.get(at), d.get(at + 1)) else {
            break;
        };
        if marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            at += 2;
            continue;
        }
        if marker == 0xd9 {
            break;
        }
        let Some(len) = be16(d, at + 2).map(usize::from) else {
            break;
        };
        let Some(body) = d.get(at + 4..at + 2 + len) else {
            break;
        };
        match marker {
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                info.height = u32::from(be16(body, 1)?);
                info.width = u32::from(be16(body, 3)?);
            }
            0xe0 if body.starts_with(b"JFIF\0") => {
                let units = *body.get(7)?;
                let (x, y) = (be16(body, 8)?, be16(body, 10)?);
                if units != 0 || (x, y) != (1, 1) {
                    jfif = positive(f64::from(x), f64::from(y));
                }
            }
            0xe1 if body.starts_with(b"Exif\0\0") && info.exif.is_none() => {
                info.exif = exif::read(&body[6..]);
            }
            0xda => break,
            _ => {}
        }
        at += 2 + len;
    }
    info.resolution = info
        .exif
        .as_ref()
        .and_then(|e| e.resolution)
        .and_then(|(x, y)| positive(x, y))
        .or(jfif);
    Some(info)
}

fn gif(d: &[u8]) -> Option<ImageInfo> {
    let mut info = ImageInfo {
        width: u32::from(le16(d, 6)?),
        height: u32::from(le16(d, 8)?),
        ..ImageInfo::default()
    };
    let table = |flags: u8| {
        if flags & 0x80 != 0 {
            3 << ((flags & 7) + 1)
        } else {
            0
        }
    };
    let mut at = 13 + table(*d.get(10)?);
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let n = usize::from(*d.get(at)?);
            at += 1 + n;
            if n == 0 {
                return Some(at);
            }
        }
    };
    // A truncated file still has its size; transparency is what was seen.
    while let Some(&b) = d.get(at) {
        match b {
            0x21 => {
                if d.get(at + 1) == Some(&0xf9) && d.get(at + 3).is_some_and(|f| f & 1 != 0) {
                    info.alpha = true;
                }
                let Some(next) = skip_blocks(at + 2) else {
                    break;
                };
                at = next;
            }
            0x2c => {
                let Some(&flags) = d.get(at + 9) else {
                    break;
                };
                let Some(next) = skip_blocks(at + 10 + table(flags) + 1) else {
                    break;
                };
                at = next;
            }
            _ => break,
        }
    }
    Some(info)
}

fn bmp(d: &[u8]) -> Option<ImageInfo> {
    let header = le32(d, 14)?;
    if header == 12 {
        return Some(ImageInfo {
            width: u32::from(le16(d, 18)?),
            height: u32::from(le16(d, 20)?),
            ..ImageInfo::default()
        });
    }
    let width = (le32(d, 18)? as i32).unsigned_abs();
    let height = (le32(d, 22)? as i32).unsigned_abs();
    let bits = le16(d, 28)?;
    let (x, y) = (le32(d, 38)? as i32, le32(d, 42)? as i32);
    Some(ImageInfo {
        width,
        height,
        resolution: positive(f64::from(x) / 100.0, f64::from(y) / 100.0),
        alpha: bits == 32 && header >= 56 && le32(d, 66).is_some_and(|mask| mask != 0),
        exif: None,
    })
}

fn tiff(d: &[u8]) -> Option<ImageInfo> {
    let exif = exif::read(d)?;
    Some(ImageInfo {
        width: exif.width?,
        height: exif.height?,
        resolution: exif.resolution.and_then(|(x, y)| positive(x, y)),
        alpha: exif.alpha,
        exif: Some(exif),
    })
}

fn webp(d: &[u8]) -> Option<ImageInfo> {
    let mut info = ImageInfo::default();
    let mut at = 12;
    while let (Some(kind), Some(len)) = (d.get(at..at + 4), le32(d, at + 4)) {
        let len = usize::try_from(len).ok()?;
        let body = d.get(at + 8..(at + 8 + len).min(d.len()))?;
        match kind {
            b"VP8X" => {
                info.alpha = body.first()? & 0x10 != 0;
                info.width = le24(body, 4)? + 1;
                info.height = le24(body, 7)? + 1;
            }
            b"VP8 " if info.width == 0 => {
                if body.get(3..6)? != [0x9d, 0x01, 0x2a] {
                    return None;
                }
                info.width = u32::from(le16(body, 6)? & 0x3fff);
                info.height = u32::from(le16(body, 8)? & 0x3fff);
            }
            b"VP8L" if info.width == 0 => {
                if *body.first()? != 0x2f {
                    return None;
                }
                let bits = le32(body, 1)?;
                info.width = (bits & 0x3fff) + 1;
                info.height = (bits >> 14 & 0x3fff) + 1;
                info.alpha = bits >> 28 & 1 != 0;
            }
            b"EXIF" => {
                let tiff = body.strip_prefix(b"Exif\0\0").unwrap_or(body);
                info.exif = exif::read(tiff);
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_size_resolution_and_alpha() {
        let mut d = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x03\0\0\0\x02\x08\x02\0\0\0".to_vec();
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(b"\0\0\0\x09pHYs\0\0\x2e\x23\0\0\x1e\xa2\x01");
        d.extend_from_slice(&[0; 4]);
        let info = read(&d).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        // 11811 and 7842 dots per meter.
        assert_eq!(info.dpi(), (118, 78));
        assert!(!info.alpha);
    }

    #[test]
    fn jpeg_jfif_aspect_ratio_is_no_resolution() {
        let jfif = |units: u8, x: u16| {
            let mut d = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
            d.extend_from_slice(b"JFIF\0\x01\x01");
            d.push(units);
            d.extend_from_slice(&x.to_be_bytes());
            d.extend_from_slice(&x.to_be_bytes());
            d.extend_from_slice(&[0, 0]);
            d.extend_from_slice(&[0xff, 0xc0, 0, 11, 8, 0, 3, 0, 4, 1, 0, 0, 0]);
            d.extend_from_slice(&[0xff, 0xd9]);
            read(&d).unwrap()
        };
        assert_eq!(jfif(0, 1).resolution, None);
        assert_eq!(jfif(0, 300).dpi(), (300, 300));
        assert_eq!(jfif(2, 100).dpi(), (100, 100));
        assert_eq!(jfif(1, 0).resolution, None);
        assert_eq!((jfif(1, 72).width, jfif(1, 72).height), (4, 3));
    }
}
