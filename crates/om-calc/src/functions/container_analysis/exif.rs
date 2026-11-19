//! The TIFF directory structure behind EXIF metadata: JPEG `APP1 Exif`
//! segments, WebP `EXIF` chunks and TIFF files themselves. Only the tags
//! the container attributes use are read.

/// The tags of a TIFF header's first directory and its EXIF and GPS
/// sub-directories that `GetContainerAttribute` reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Exif {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Orientation (274), 1 to 8 when valid.
    pub orientation: Option<u16>,
    /// XResolution and YResolution (282, 283), whatever the unit.
    pub resolution: Option<(f64, f64)>,
    /// ExtraSamples (338) names an alpha channel.
    pub alpha: bool,
    pub make: Option<String>,
    pub model: Option<String>,
    /// DateTime (306): when the file was last changed.
    pub modified: Option<String>,
    /// DateTimeDigitized (36868, EXIF directory).
    pub digitized: Option<String>,
    /// GPSLatitude, GPSLongitude with their references, as signed degrees.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

const IMAGE_WIDTH: u16 = 256;
const IMAGE_LENGTH: u16 = 257;
const MAKE: u16 = 271;
const MODEL: u16 = 272;
const ORIENTATION: u16 = 274;
const X_RESOLUTION: u16 = 282;
const Y_RESOLUTION: u16 = 283;
const DATE_TIME: u16 = 306;
const EXTRA_SAMPLES: u16 = 338;
const EXIF_IFD: u16 = 34665;
const GPS_IFD: u16 = 34853;
const DATE_TIME_DIGITIZED: u16 = 36868;
const GPS_LATITUDE_REF: u16 = 1;
const GPS_LATITUDE: u16 = 2;
const GPS_LONGITUDE_REF: u16 = 3;
const GPS_LONGITUDE: u16 = 4;

/// A TIFF stream: byte order and the bytes from the `II`/`MM` header on.
struct Tiff<'a> {
    data: &'a [u8],
    big_endian: bool,
}

/// One directory entry.
#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// The value field's offset in the stream (the value itself when it
    /// fits in four bytes, else a pointer to it).
    field: usize,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Tiff<'a>> {
        let big_endian = match data.get(..4)? {
            [b'I', b'I', 42, 0] => false,
            [b'M', b'M', 0, 42] => true,
            _ => return None,
        };
        Some(Tiff { data, big_endian })
    }

    fn u16_at(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    /// The entries of the directory at `offset`.
    fn directory(&self, offset: usize) -> Vec<Entry> {
        let Some(n) = self.u16_at(offset) else {
            return Vec::new();
        };
        (0..usize::from(n))
            .map_while(|i| {
                let at = offset + 2 + i * 12;
                Some(Entry {
                    tag: self.u16_at(at)?,
                    kind: self.u16_at(at + 2)?,
                    count: self.u32_at(at + 4)?,
                    field: at + 8,
                })
            })
            .collect()
    }

    /// The bytes of an entry's value.
    fn value(&self, e: &Entry) -> Option<&'a [u8]> {
        let unit = match e.kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        };
        let len = unit * usize::try_from(e.count).ok()?;
        let at = if len <= 4 {
            e.field
        } else {
            usize::try_from(self.u32_at(e.field)?).ok()?
        };
        self.data.get(at..at.checked_add(len)?)
    }

    /// The first value of a SHORT or LONG entry.
    fn integer(&self, e: &Entry) -> Option<u32> {
        match e.kind {
            3 => self.u16_at(e.field).map(u32::from),
            4 => self.u32_at(e.field),
            _ => None,
        }
    }

    /// The `index`th value of a RATIONAL entry.
    fn rational(&self, e: &Entry, index: usize) -> Option<f64> {
        if e.kind != 5 || index >= usize::try_from(e.count).ok()? {
            return None;
        }
        let at = usize::try_from(self.u32_at(e.field)?).ok()? + index * 8;
        let (n, d) = (self.u32_at(at)?, self.u32_at(at + 4)?);
        (d != 0).then(|| f64::from(n) / f64::from(d))
    }

    /// An ASCII entry up to its first NUL.
    fn ascii(&self, e: &Entry) -> Option<String> {
        if e.kind != 2 {
            return None;
        }
        let bytes = self.value(e)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    fn degrees(&self, value: Option<&Entry>, reference: Option<&Entry>) -> Option<f64> {
        let value = value?;
        let d = self.rational(value, 0)?;
        let m = self.rational(value, 1).unwrap_or(0.0);
        let s = self.rational(value, 2).unwrap_or(0.0);
        let degrees = d + m / 60.0 + s / 3600.0;
        let negative = reference
            .and_then(|r| self.ascii(r))
            .is_some_and(|r| r.starts_with(['S', 'W']));
        Some(if negative { -degrees } else { degrees })
    }
}

/// Reads the tags from a TIFF stream (`II*\0` or `MM\0*` first).
pub(super) fn read(data: &[u8]) -> Option<Exif> {
    let tiff = Tiff::new(data)?;
    let first = usize::try_from(tiff.u32_at(4)?).ok()?;
    let mut exif = Exif::default();
    let mut sub = None;
    let mut gps = None;
    for e in tiff.directory(first) {
        match e.tag {
            IMAGE_WIDTH => exif.width = tiff.integer(&e),
            IMAGE_LENGTH => exif.height = tiff.integer(&e),
            MAKE => exif.make = tiff.ascii(&e),
            MODEL => exif.model = tiff.ascii(&e),
            ORIENTATION => exif.orientation = tiff.integer(&e).and_then(|v| u16::try_from(v).ok()),
            X_RESOLUTION => {
                let x = tiff.rational(&e, 0);
                exif.resolution = x.map(|x| (x, exif.resolution.map_or(x, |r| r.1)));
            }
            Y_RESOLUTION => {
                if let Some(y) = tiff.rational(&e, 0) {
                    exif.resolution = Some((exif.resolution.map_or(y, |r| r.0), y));
                }
            }
            DATE_TIME => exif.modified = tiff.ascii(&e),
            EXTRA_SAMPLES => exif.alpha = matches!(tiff.integer(&e), Some(1 | 2)),
            EXIF_IFD => sub = tiff.integer(&e),
            GPS_IFD => gps = tiff.integer(&e),
            _ => {}
        }
    }
    if let Some(offset) = sub.and_then(|o| usize::try_from(o).ok()) {
        if let Some(e) = tiff
            .directory(offset)
            .into_iter()
            .find(|e| e.tag == DATE_TIME_DIGITIZED)
        {
            exif.digitized = tiff.ascii(&e);
        }
    }
    if let Some(offset) = gps.and_then(|o| usize::try_from(o).ok()) {
        let entries = tiff.directory(offset);
        let find = |tag| entries.iter().find(|e| e.tag == tag);
        exif.latitude = tiff.degrees(find(GPS_LATITUDE), find(GPS_LATITUDE_REF));
        exif.longitude = tiff.degrees(find(GPS_LONGITUDE), find(GPS_LONGITUDE_REF));
    }
    Some(exif)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A big-endian TIFF header with one directory of `entries` (tag, type,
    /// count, value field) and `extra` bytes after it.
    fn tiff(entries: &[(u16, u16, u32, [u8; 4])], extra: &[u8]) -> Vec<u8> {
        let mut out = b"MM\0\x2a\0\0\0\x08".to_vec();
        out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (tag, kind, count, value) in entries {
            out.extend_from_slice(&tag.to_be_bytes());
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(value);
        }
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(extra);
        out
    }

    #[test]
    fn reads_orientation_resolution_and_make() {
        // Directory at 8 with 3 entries ends at 8 + 2 + 36 + 4 = 50.
        let mut rational = 150u32.to_be_bytes().to_vec();
        rational.extend_from_slice(&1u32.to_be_bytes());
        rational.extend_from_slice(b"Apple\0");
        let data = tiff(
            &[
                (ORIENTATION, 3, 1, [0, 6, 0, 0]),
                (X_RESOLUTION, 5, 1, 50u32.to_be_bytes()),
                (MAKE, 2, 6, 58u32.to_be_bytes()),
            ],
            &rational,
        );
        let exif = read(&data).unwrap();
        assert_eq!(exif.orientation, Some(6));
        assert_eq!(exif.resolution, Some((150.0, 150.0)));
        assert_eq!(exif.make.as_deref(), Some("Apple"));
        assert_eq!(read(b"XX\0*"), None);
    }
}
