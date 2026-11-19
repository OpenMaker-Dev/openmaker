//! `ReadQRCode`: the leftmost QR code found in a picture, its bytes read as
//! FileMaker Server reads them (oracle): UTF-8 after a byte order mark,
//! else the first of Big5, Shift_JIS, UTF-8 and ISO 8859-1 that reads the
//! bytes without error. So UTF-8 `café` (`63 61 66 c3 a9`) comes back as
//! Big5 `caf矇`, UTF-8 `日本` as Shift_JIS `譌･譛ｬ`, and `Ā` (`c4 80`, no
//! Big5 or Shift_JIS reading) as UTF-8. Big5 is the original table with
//! leads 0xA1-0xF9 and its user-defined rows 0xC6A1-0xC8FE in the private
//! use area from U+F6B1 (`c7 d1` is U+F77E). Kanji segments are Shift_JIS
//! already. Line breaks stay as encoded (CR LF, LF, CR).

use image::DynamicImage;

/// The text of the first QR code in the picture; `None` when the data does
/// not decode as a picture or holds no readable code.
pub(super) fn read(data: &[u8]) -> Option<String> {
    let picture = image::load_from_memory(data).ok()?;
    // Transparent pixels are white paper.
    let grey = on_white(&picture);
    let (w, h) = grey.dimensions();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(w as usize, h as usize, |x, y| {
            grey.get_pixel(x as u32, y as u32)[0]
        });
    // Of several codes, the leftmost (then topmost): the server read the
    // left one of two side by side.
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| {
            let mut bytes = Vec::new();
            grid.decode_to(&mut bytes).ok()?;
            let left = grid.bounds.iter().map(|p| p.x).min()?;
            let top = grid.bounds.iter().map(|p| p.y).min()?;
            Some(((left, top), bytes))
        })
        .min_by_key(|(corner, _)| *corner)
        .map(|(_, bytes)| text(&bytes))
}

fn on_white(picture: &DynamicImage) -> image::GrayImage {
    let rgba = picture.to_rgba8();
    let mut grey = image::GrayImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let [r, g, b, a] = p.0;
        let luma = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
        let a = u32::from(a);
        let v = (luma * a + 255 * (255 - a)) / 255;
        grey.put_pixel(x, y, image::Luma([v as u8]));
    }
    grey
}

/// The text of a code's bytes.
pub(super) fn text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        if let Ok(s) = std::str::from_utf8(rest) {
            return s.to_string();
        }
    }
    if let Some(s) = big5(bytes) {
        return s;
    }
    if shift_jis_shaped(bytes) {
        let (s, malformed) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(bytes);
        if !malformed {
            return s.into_owned();
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// Single bytes and lead/trail pairs of Shift_JIS proper: unlike the web's
/// Shift_JIS decoder, a lone 0x80 or 0xA0 is no character (`c4 80 62` is
/// UTF-8 `Āb`, not Shift_JIS).
fn shift_jis_shaped(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x00..=0x7f | 0xa1..=0xdf => i += 1,
            0x81..=0x9f | 0xe0..=0xfc => {
                if !matches!(bytes.get(i + 1), Some(0x40..=0x7e | 0x80..=0xfc)) {
                    return false;
                }
                i += 2;
            }
            _ => return false,
        }
    }
    true
}

fn big5(bytes: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let lead = bytes[i];
        if lead < 0x80 {
            out.push(char::from(lead));
            i += 1;
            continue;
        }
        let trail = *bytes.get(i + 1)?;
        if !(0xa1..=0xf9).contains(&lead) || !matches!(trail, 0x40..=0x7e | 0xa1..=0xfe) {
            return None;
        }
        let code = u16::from_be_bytes([lead, trail]);
        if (0xc6a1..=0xc8fe).contains(&code) {
            // 157 cells a row: 0x40-0x7E, then 0xA1-0xFE.
            let column = if trail <= 0x7e {
                trail - 0x40
            } else {
                trail - 0xa1 + 63
            };
            let cell = u32::from(lead - 0xc6) * 157 + u32::from(column);
            out.push(char::from_u32(0xf6b1 - 63 + cell)?);
        } else {
            let (s, malformed) = encoding_rs::BIG5.decode_without_bom_handling(&bytes[i..i + 2]);
            if malformed {
                return None;
            }
            out.push_str(&s);
        }
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_the_encoding_as_the_server_does() {
        assert_eq!(text(b"caf\xc3\xa9"), "caf矇");
        assert_eq!(text(b"caf\xe9"), "café");
        assert_eq!(text(b"\xc4\x80b"), "Āb");
        assert_eq!(text(b"\xef\xbb\xbfGr\xc3\xbc\xc3\x9fe"), "Grüße");
        assert_eq!(text(b"\x93\xfa\x96\x7b"), "日本");
        assert_eq!(text(b"\xe6\x97\xa5\xe6\x9c\xac"), "譌･譛ｬ");
        assert_eq!(text(b"\xa4\xa4"), "中");
        assert_eq!(text(b"\xc7\xd1\xb1\xdb"), "\u{f77e}旋");
        assert_eq!(text(b"\x80\x81"), "\u{80}\u{81}");
        assert_eq!(text(b"a\r\nb"), "a\r\nb");
    }
}
