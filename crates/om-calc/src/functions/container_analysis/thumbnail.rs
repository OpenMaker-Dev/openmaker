//! `GetThumbnail`'s pictures (oracle). The box is the arguments' absolute
//! values, fractions kept; a picture that fits it, unturned, is returned
//! as it is (never enlarged). Otherwise the picture is scaled to fit,
//! keeping its proportions: the limiting side takes the box's size and the
//! other its share, each rounded half up and at least 1 (3 x 2 in a 2.4
//! box is 2 x 2: 2.4 and 1.6 rounded). EXIF orientation is applied, so a
//! turned photo always gets a new picture. JPEG pictures give a JPEG named
//! `<name>.jpg`, everything else (PNG, GIF, BMP, TIFF, WebP, PDF pages) a
//! PNG named `<name>.png`, the name being the original's without its
//! extension, with `SIZE` and a 72 dpi `DPI_` stream.

use std::io::Cursor;

use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageFormat};

use crate::value::{Container, StreamType};
use crate::Text;

use super::picture::ImageInfo;

/// The thumbnail size for a picture of `w` x `h` in a `bw` x `bh` box, or
/// `None` when it fits.
pub(super) fn fit(w: u32, h: u32, bw: f64, bh: f64) -> Option<(u32, u32)> {
    let (w, h) = (f64::from(w), f64::from(h));
    if bw >= w && bh >= h {
        return None;
    }
    let round = |x: f64| ((x + 0.5).floor().max(1.0)).min(f64::from(u32::MAX)) as u32;
    Some(if bw / w <= bh / h {
        (round(bw), round(h * bw / w))
    } else {
        (round(w * bh / h), round(bh))
    })
}

/// The name of a thumbnail of the file `name`.
pub(super) fn name(name: &str, jpeg: bool) -> String {
    let stem = name.rfind('.').map_or(name, |dot| &name[..dot]);
    format!("{stem}.{}", if jpeg { "jpg" } else { "png" })
}

/// A picture scaled to `w` x `h`: the data decoded, turned as its EXIF
/// orientation says, scaled and encoded; `None` when it does not decode.
pub(super) fn scale(data: &[u8], info: &ImageInfo, w: u32, h: u32, jpeg: bool) -> Option<Vec<u8>> {
    let mut picture = image::load_from_memory(data).ok()?;
    if let Some(o) = info
        .orientation()
        .and_then(|o| Orientation::from_exif(o as u8))
    {
        picture.apply_orientation(o);
    }
    let picture = picture.resize_exact(w, h, FilterType::Lanczos3);
    let picture = if info.alpha && !jpeg {
        DynamicImage::ImageRgba8(picture.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(picture.to_rgb8())
    };
    encode(&picture, jpeg)
}

pub(super) fn encode(picture: &DynamicImage, jpeg: bool) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    if jpeg {
        picture
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out, 90,
            ))
            .ok()?;
    } else {
        picture.write_to(&mut out, ImageFormat::Png).ok()?;
    }
    Some(out.into_inner())
}

/// The thumbnail container.
pub(super) fn container(name: String, data: Vec<u8>, w: u32, h: u32, jpeg: bool) -> Container {
    let pair = |a: u32, b: u32| {
        let mut v = (a.min(0xffff) as u16).to_be_bytes().to_vec();
        v.extend_from_slice(&(b.min(0xffff) as u16).to_be_bytes());
        v
    };
    let kind = StreamType(if jpeg { *b"JPEG" } else { *b"PNGf" });
    Container {
        text: Text::plain(name),
        streams: vec![
            (kind, data),
            (StreamType(*b"SIZE"), pair(w, h)),
            (StreamType(*b"DPI_"), pair(72, 72)),
        ],
        path_type: Some("image"),
        ..Container::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_rounding_half_up() {
        assert_eq!(fit(3, 2, 30.0, 30.0), None);
        assert_eq!(fit(3, 2, 3.0, 2.0), None);
        assert_eq!(fit(3, 2, 2.0, 2.0), Some((2, 1)));
        assert_eq!(fit(3, 2, 2.4, 2.4), Some((2, 2)));
        assert_eq!(fit(3, 2, 2.5, 2.5), Some((3, 2)));
        assert_eq!(fit(33, 100, 50.0, 50.0), Some((17, 50)));
        assert_eq!(fit(100, 30, 45.0, 45.0), Some((45, 14)));
        assert_eq!(fit(640, 480, 101.0, 101.0), Some((101, 76)));
        assert_eq!(fit(640, 480, 1.0, 1000.0), Some((1, 1)));
        assert_eq!(fit(3, 2, 0.4, 5.0), Some((1, 1)));
        assert_eq!(fit(3, 2, 100.0, 1.0), Some((2, 1)));
    }

    #[test]
    fn names() {
        assert_eq!(name("a.JPEG", true), "a.jpg");
        assert_eq!(name("a.b.gif", false), "a.b.png");
        assert_eq!(name(".gif", false), ".png");
        assert_eq!(name("a", false), "a.png");
    }
}
