//! `GetContainerAttribute` (oracle answers in calc-functions.md 9, "Oracle
//! answers (container analysis)").
//!
//! Names match without spaces and case (`" all"`, `"dpi width"`, `"File
//! Size"`); anything else is `?`. A group gives `Label: value` lines
//! joined by returns (`Label:` when the value is empty), `all` every group
//! that applies, each headed `[Group]`, separated by an empty line; a group
//! that does not apply is empty text. Single attributes have their type:
//! numbers for sizes, dimensions, resolution, transparency and bit rate,
//! timestamps for `created` and `modified`, a time for `duration`, a
//! container for `coverArt`, text for the rest. One that does not apply is
//! `?`, except `barcodeText`, `barcodeType` and `externalFiles` (empty
//! text) and the photo attributes of a JPEG without them (empty).

use md5::{Digest, Md5};

use crate::datetime::{Date, Time, Timestamp};
use crate::error::CalcError;
use crate::number::Number;
use crate::value::{Container, Ctx, Value, ValueType};

use super::audio::{self, Audio};
use super::media::{Kind, Media, Subject};
use super::pdf_worker;
use super::picture::ImageInfo;

const GROUPS: [&str; 6] = ["general", "image", "photo", "audio", "barcode", "signature"];

/// An attribute name as it matches: without spaces, lowercase.
pub(super) fn key(name: &str) -> String {
    name.chars()
        .filter(|&c| c != ' ')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The value of attribute `name` of `subject`; error 3 when it is a PDF's
/// size and no worker can read PDFs ([`pdf_worker`]).
pub(super) fn get(subject: &Subject, name: &str, cx: &Ctx) -> Result<Value, CalcError> {
    let key = key(name);
    let facts = match subject {
        Subject::Empty => return Ok(Value::invalid()),
        _ => Facts::of(subject),
    };
    let value = facts.value(&key, cx);
    if facts.unavailable.get() {
        return Err(pdf_worker::Unavailable.into());
    }
    Ok(value)
}

impl Facts<'_> {
    fn value(&self, key: &str, cx: &Ctx) -> Value {
        if key == "all" {
            let groups: Vec<String> = GROUPS
                .iter()
                .filter_map(|g| {
                    let body = self.group(g, cx)?;
                    Some(format!("[{}]\r{body}", title(g)))
                })
                .collect();
            return Value::text(groups.join("\r\r"));
        }
        if GROUPS.contains(&key) {
            return Value::text(self.group(key, cx).unwrap_or_default());
        }
        match self.attribute(key, cx) {
            Some(v) => v.unwrap_or_else(|| match key {
                "barcodetext" | "barcodetype" | "externalfiles" => Value::empty_text(),
                _ => Value::invalid(),
            }),
            None => Value::invalid(),
        }
    }
}

fn title(group: &str) -> &'static str {
    match group {
        "general" => "General",
        "image" => "Image",
        "photo" => "Photo",
        "audio" => "Audio",
        "barcode" => "Barcode",
        _ => "Signature",
    }
}

/// What the attributes are computed from.
struct Facts<'a> {
    general: General,
    media: Option<&'a Media<'a>>,
    picture: Option<ImageInfo>,
    audio: Option<Audio>,
    /// A PDF's size was asked for and no worker could read it: the
    /// function fails with error 3 ([`pdf_worker`]).
    unavailable: std::cell::Cell<bool>,
}

struct General {
    name: String,
    storage: &'static str,
    md5: String,
    file_size: Option<usize>,
    internal_size: Option<usize>,
}

fn md5_hex(data: &[u8]) -> String {
    Md5::digest(data)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

impl<'a> Facts<'a> {
    fn of(subject: &'a Subject<'a>) -> Facts<'a> {
        match subject {
            Subject::Data(media) => Facts {
                general: General {
                    name: media.name.clone(),
                    storage: "Embedded",
                    md5: md5_hex(media.container.stored_data()),
                    file_size: Some(media.data.len()),
                    internal_size: Some(media.internal_size()),
                },
                media: Some(media),
                picture: media.picture(),
                audio: match media.kind {
                    Kind::Mp3 => audio::mp3(media.data),
                    Kind::M4a => audio::m4a(media.data),
                    _ => None,
                },
                unavailable: Default::default(),
            },
            // A file reference without data (unverified: no oracle value
            // holds one).
            Subject::Reference(c) => Facts {
                general: General {
                    name: c.text.as_str().to_string(),
                    storage: "File Reference",
                    md5: String::new(),
                    file_size: None,
                    internal_size: None,
                },
                media: None,
                picture: None,
                audio: None,
                unavailable: Default::default(),
            },
            Subject::Text(text) => Facts {
                general: General {
                    name: String::new(),
                    storage: "Text",
                    md5: md5_hex(text.as_bytes()),
                    file_size: None,
                    internal_size: Some(text.encode_utf16().count()),
                },
                media: None,
                picture: None,
                audio: None,
                unavailable: Default::default(),
            },
            Subject::Empty => unreachable!("empty values have no attributes"),
        }
    }

    fn kind(&self) -> Option<Kind> {
        self.media.map(|m| m.kind)
    }

    /// The attributes of a group in order, as (label, text); `None` when
    /// the group does not apply.
    fn group(&self, group: &str, cx: &Ctx) -> Option<String> {
        let names: &[(&str, &str)] = match group {
            "general" => &[
                ("Filename", "filename"),
                ("Storage Type", "storagetype"),
                ("MD5", "md5"),
                ("File Size", "filesize"),
                ("Internal Size", "internalsize"),
                ("External Size", "externalsize"),
                ("External Files", "externalfiles"),
            ],
            "image" if self.kind() == Some(Kind::Pdf) => {
                &[("Width", "width"), ("Height", "height")]
            }
            "image" if self.kind().is_some_and(Kind::is_picture) => &[
                ("Width", "width"),
                ("Height", "height"),
                ("DPI Width", "dpiwidth"),
                ("DPI Height", "dpiheight"),
                ("Transparency", "transparency"),
            ],
            "photo" if self.kind().is_some_and(Kind::is_photo) => &[
                ("Orientation", "orientation"),
                ("Created", "created"),
                ("Modified", "modified"),
                ("Latitude", "latitude"),
                ("Longitude", "longitude"),
                ("Make", "make"),
                ("Model", "model"),
            ],
            "audio" if self.audio.is_some() => &[
                ("Title", "title"),
                ("Artist", "artist"),
                ("Album", "album"),
                ("Year", "year"),
                ("Track", "track"),
                ("Genre", "genre"),
                ("Composer", "composer"),
                ("Cover Art", "coverart"),
                ("Duration", "duration"),
                ("Bit Rate", "bitrate"),
            ],
            "barcode" if self.barcode().is_some() => &[
                ("Barcode Text", "barcodetext"),
                ("Barcode Type", "barcodetype"),
            ],
            _ => return None,
        };
        let lines: Vec<String> = names
            .iter()
            .map(|(label, key)| {
                let text = self.shown(key, cx);
                if text.is_empty() {
                    format!("{label}:")
                } else {
                    format!("{label}: {text}")
                }
            })
            .collect();
        Some(lines.join("\r"))
    }

    /// An attribute as a group line shows it.
    fn shown(&self, key: &str, cx: &Ctx) -> String {
        match key {
            "externalfiles" => "0".into(),
            "transparency" => match self.picture.as_ref() {
                Some(p) if p.alpha => "1 (True)".into(),
                _ => "0 (False)".into(),
            },
            "coverart" => match self.audio.as_ref().and_then(|a| a.cover.as_ref()) {
                Some((_, true)) => "png".into(),
                Some((_, false)) => "jpg".into(),
                None => String::new(),
            },
            "bitrate" => match self.audio.as_ref() {
                Some(a) if a.vbr => format!("{} kbps (VBR)", a.kbps),
                Some(a) => format!("{} kbps", a.kbps),
                None => String::new(),
            },
            _ => match self.attribute(key, cx) {
                Some(Some(v)) => v.to_text(cx).into_string(),
                _ => "?".into(),
            },
        }
    }

    /// `Some(Some(v))` an attribute's value, `Some(None)` an attribute that
    /// does not apply, `None` an unknown name.
    fn attribute(&self, key: &str, cx: &Ctx) -> Option<Option<Value>> {
        let count = |n: usize| Value::number(n as i64);
        let g = &self.general;
        let kind = self.kind();
        let picture = kind.is_some_and(Kind::is_picture);
        let photo = kind.is_some_and(Kind::is_photo);
        let exif = self.picture.as_ref().and_then(|p| p.exif.as_ref());
        let audio = self.audio.as_ref();
        let text = |s: Option<&String>| Value::text(s.cloned().unwrap_or_default());
        Some(match key {
            "filename" => Some(Value::text(g.name.clone())),
            "storagetype" => Some(Value::text(g.storage)),
            "md5" => Some(Value::text(g.md5.clone())),
            "filesize" => g.file_size.map(count),
            "internalsize" => g.internal_size.map(count),
            "externalsize" => Some(count(0)),
            "externalfiles" => Some(Value::empty_text()),
            "width" | "height" => {
                let size = match kind {
                    Some(Kind::Pdf) => self.media.and_then(|m| {
                        pdf_worker::page_size(m.data).unwrap_or_else(|_| {
                            self.unavailable.set(true);
                            None
                        })
                    }),
                    Some(_) if picture => self
                        .picture
                        .as_ref()
                        .map(ImageInfo::display_size)
                        .or_else(|| self.media.and_then(Media::stored_size)),
                    _ => None,
                };
                if kind.is_some_and(|k| k.is_picture() || k == Kind::Pdf) {
                    Some(match size {
                        Some((w, h)) => count(if key == "width" { w } else { h } as usize),
                        None => Value::invalid(),
                    })
                } else {
                    None
                }
            }
            "dpiwidth" | "dpiheight" if picture => {
                let (x, y) = self
                    .picture
                    .as_ref()
                    .map(ImageInfo::dpi)
                    .or_else(|| self.media.and_then(Media::stored_dpi))
                    .unwrap_or((72, 72));
                Some(count(if key == "dpiwidth" { x } else { y } as usize))
            }
            "transparency" if picture => Some(Value::boolean(
                self.picture.as_ref().is_some_and(|p| p.alpha),
            )),
            "orientation" if photo => Some(Value::text(orientation(
                self.picture.as_ref().and_then(ImageInfo::orientation),
            ))),
            "created" if photo => Some(
                exif.and_then(|e| e.digitized.as_deref())
                    .map_or(Value::empty(ValueType::Timestamp), exif_time),
            ),
            "modified" if photo => Some(modified(
                exif.and_then(|e| e.digitized.as_deref()),
                exif.and_then(|e| e.modified.as_deref()),
            )),
            "latitude" | "longitude" if photo => {
                let v = exif.and_then(|e| {
                    if key == "latitude" {
                        e.latitude
                    } else {
                        e.longitude
                    }
                });
                Some(Value::text(
                    v.map(|d| format!("{d:.6}")).unwrap_or_default(),
                ))
            }
            "make" if photo => Some(text(exif.and_then(|e| e.make.as_ref()))),
            "model" if photo => Some(text(exif.and_then(|e| e.model.as_ref()))),
            "title" => audio.map(|a| text(a.title.as_ref())),
            "artist" => audio.map(|a| text(a.artist.as_ref())),
            "album" => audio.map(|a| text(a.album.as_ref())),
            "year" => audio.map(|a| Value::text(year(a.year.as_deref(), cx))),
            "track" => audio.map(|a| text(a.track.as_ref())),
            "genre" => audio.map(|a| text(a.genre.as_ref())),
            "composer" => audio.map(|a| text(a.composer.as_ref())),
            "coverart" => audio.and_then(|a| {
                let (data, png) = a.cover.as_ref()?;
                let name = if *png { "Untitled.png" } else { "Untitled.jpg" };
                Some(Value::Container(Container::file(name, data.clone())))
            }),
            "duration" => audio.and_then(|a| {
                Time::from_seconds(&Number::from_i64(a.seconds as i64)).map(Value::Time)
            }),
            "bitrate" => audio.map(|a| count(a.kbps as usize)),
            "barcodetext" => self.barcode().map(|(t, _)| Value::text(t)),
            "barcodetype" => self.barcode().map(|(_, t)| Value::text(t)),
            "signed" => None,
            "orientation" | "created" | "modified" | "latitude" | "longitude" | "make"
            | "model" | "dpiwidth" | "dpiheight" | "transparency" => None,
            _ => return None,
        })
    }

    /// The barcode a device inserted (`BAR#` text, `BART` type; their
    /// format is not established, containers.md 2.4: read as UTF-8).
    fn barcode(&self) -> Option<(String, String)> {
        let m = self.media?;
        let text = m.stream(b"BAR#")?;
        let kind = m.stream(b"BART").unwrap_or_default();
        Some((
            String::from_utf8_lossy(text).into_owned(),
            String::from_utf8_lossy(kind).into_owned(),
        ))
    }
}

/// The orientation text: FileMaker's code for the EXIF value and its name.
/// The server maps EXIF 2 and 4 to each other and both quarter turns (6
/// and 8) to 8; no value or 0 is normal, values past 8 show nothing.
fn orientation(exif: Option<u16>) -> String {
    let code = match exif {
        None | Some(0 | 1) => 1,
        Some(2) => 4,
        Some(3) => 3,
        Some(4) => 2,
        Some(5) => 5,
        Some(6 | 8) => 8,
        Some(7) => 7,
        Some(_) => return String::new(),
    };
    let name = match code {
        1 => "Normal",
        2 => "Flipped horizontally",
        3 => "Rotated 180 degrees",
        4 => "Flipped vertically",
        5 => "Rotated 90 degrees counterclockwise and flipped vertically",
        6 => "Rotated 90 degrees counterclockwise",
        7 => "Rotated 90 degrees clockwise and flipped vertically",
        _ => "Rotated 90 degrees clockwise",
    };
    format!("{code} ({name})")
}

/// An EXIF date (`YYYY:MM:DD HH:MM:SS`) as a timestamp. The date keeps the
/// separator it was written with, `:` becoming `/` (`2020:01:15` shows
/// `15/01/2020` in a German file, `2021-03-04` `04-03-2021`); impossible
/// values and other text are `?`, except a bare date, which stays text.
fn exif_time(s: &str) -> Value {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let date_shaped = b.len() >= 10
        && b[4] == b[7]
        && !b[4].is_ascii_digit()
        && digits(0..4).is_some()
        && digits(5..7).is_some()
        && digits(8..10).is_some();
    if b.len() == 10 && date_shaped {
        return Value::text(s);
    }
    if b.len() != 19 || !date_shaped || b[10] != b' ' || b[13] != b':' || b[16] != b':' {
        return Value::invalid();
    }
    let sep = if b[4] == b':' { '/' } else { char::from(b[4]) };
    let parts = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    );
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = parts else {
        return Value::invalid();
    };
    if h > 23 || mi > 59 || se > 59 {
        return Value::invalid();
    }
    let Some(date) = Date::from_ymd(i64::from(y), mo, d) else {
        return Value::invalid();
    };
    let seconds = i64::from(h * 3600 + mi * 60 + se);
    let time = Time::from_seconds(&Number::from_i64(seconds)).expect("a time of day");
    match Timestamp::from_date_time(date, time) {
        Some(t) => Value::Timestamp(t.with_separator(Some(sep))),
        None => Value::invalid(),
    }
}

/// `Modified` is shown when it is later than `Created`, or when only one
/// of them reads (a `?` included); nothing when both are `?` or it is not
/// later.
fn modified(created: Option<&str>, modified: Option<&str>) -> Value {
    let none = Value::empty(ValueType::Timestamp);
    let Some(m) = modified.map(exif_time) else {
        return none;
    };
    let Some(c) = created.map(exif_time) else {
        return m;
    };
    match (&c, &m) {
        (Value::Timestamp(c), Value::Timestamp(m)) if m.micros() <= c.micros() => none,
        _ if c.is_invalid() && m.is_invalid() => none,
        _ => m,
    }
}

/// A tagged year: a full date `YYYY-MM-DD...` shows as `YYYY (<date>)` in
/// the file's date format.
fn year(tagged: Option<&str>, cx: &Ctx) -> String {
    let Some(t) = tagged else {
        return String::new();
    };
    let field = |r: std::ops::Range<usize>| t.get(r).and_then(|s| s.parse::<u32>().ok());
    if t.len() >= 10 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-' {
        if let (Some(y), Some(m), Some(d)) = (field(0..4), field(5..7), field(8..10)) {
            if let Some(date) = Date::from_ymd(i64::from(y), m, d) {
                return format!("{y} ({})", date.format(cx.locale));
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Locale;

    #[test]
    fn exif_dates() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        let show = |s| exif_time(s).to_text(&cx).into_string();
        assert_eq!(show("2020:01:15 13:02:03"), "1/15/2020 1:02:03 PM");
        assert_eq!(show("2021-03-04 05:06:07"), "3-4-2021 5:06:07 AM");
        assert_eq!(show("2021:13:44 05:06:07"), "?");
        assert_eq!(show("    :  :     :  :  "), "?");
        assert_eq!(show("garbage"), "?");
        assert_eq!(show("2021:03:04"), "2021:03:04");
        let later = |c, m| modified(c, m).to_text(&cx).into_string();
        assert_eq!(
            later(None, Some("2021:03:04 05:06:07")),
            "3/4/2021 5:06:07 AM"
        );
        assert_eq!(
            later(Some("2020:01:15 13:02:03"), Some("2019:01:01 00:00:00")),
            ""
        );
        assert_eq!(
            later(Some("garbage"), Some("2021:03:04 05:06:07")),
            "3/4/2021 5:06:07 AM"
        );
        assert_eq!(later(Some("garbage"), Some("2021:13:44 05:06:07")), "");
        assert_eq!(later(None, Some("garbage")), "?");
    }

    #[test]
    fn orientations() {
        assert_eq!(orientation(None), "1 (Normal)");
        assert_eq!(orientation(Some(2)), "4 (Flipped vertically)");
        assert_eq!(orientation(Some(6)), "8 (Rotated 90 degrees clockwise)");
        assert_eq!(orientation(Some(9)), "");
    }
}
