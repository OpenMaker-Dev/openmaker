//! What FileMaker makes of a value handed to a container analysis function:
//! empty, text, or container data of a kind.
//!
//! Data that a calculation decoded (`Base64Decode`, `HexDecode`,
//! `TextEncode`: one `FILE` stream and a file name) is typed as FileMaker
//! types a file it stores (oracle): by the name's extension, any case,
//! after the last `.`. `jpg`, `jpeg`, `jpe`, `jfif`, `png` and `gif` are
//! pictures whatever the bytes, `pdf` a PDF, `mp3` and `m4a` audio, `wav`,
//! `aif`, `aiff`, `au`, `mov`, `qt`, `mp4`, `m4v`, `avi`, `wmv`, `wma`,
//! `mpg`, `mpeg` other media; `bmp`, `tif`, `tiff` and `webp` are pictures
//! only when the bytes are one (FileMaker converts them); everything else
//! (`jp2`, `psd`, `svg`, `heic` on the server, `txt`, no extension) is a
//! plain file. Data the host loaded from a field keeps its stored streams.

use crate::value::{Container, StreamType, Value};

use super::picture::{self, ImageInfo};

/// How the data is stored and shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// A JPEG picture: the photo attributes apply.
    Jpeg,
    Png,
    Gif,
    /// Pictures FileMaker converts on insertion (BMP, TIFF, WebP); TIFF
    /// and WebP have photo attributes.
    Bmp,
    Tiff,
    Webp,
    Pdf,
    Mp3,
    M4a,
    /// Other sound and movie files: no analysis.
    Media,
    /// Any other file.
    File,
}

impl Kind {
    pub fn is_picture(self) -> bool {
        matches!(
            self,
            Kind::Jpeg | Kind::Png | Kind::Gif | Kind::Bmp | Kind::Tiff | Kind::Webp
        )
    }

    /// Pictures with `[Photo]` attributes.
    pub fn is_photo(self) -> bool {
        matches!(self, Kind::Jpeg | Kind::Tiff | Kind::Webp)
    }

    /// The path type FileMaker names the file with (`FNAM`, containers.md
    /// 2.4): `image:`, `movie:` or `file:`.
    pub fn path_type(self) -> &'static str {
        match self {
            k if k.is_picture() => "image",
            Kind::Mp3 | Kind::M4a | Kind::Media => "movie",
            _ => "file",
        }
    }

    pub fn of_stream(t: StreamType) -> Kind {
        match &t.0 {
            b"JPEG" | b"jpeg" => Kind::Jpeg,
            b"PNGf" | b"pngF" => Kind::Png,
            b"GIFf" | b"gifF" => Kind::Gif,
            b"BMPf" | b"bmpF" => Kind::Bmp,
            b"TIFF" | b"tiff" => Kind::Tiff,
            b"WEBP" | b"webp" => Kind::Webp,
            b"PDF " | b"pdf " => Kind::Pdf,
            b"MP3 " | b"mp3 " => Kind::Mp3,
            b"M4A " | b"m4a " => Kind::M4a,
            b"MOV " | b"mov " | b"MooV" | b"moov" | b"MPEG" | b"mpeg" | b"MPG4" | b"mpg4"
            | b"AVI " | b"avi " | b"WMV " | b"wmv " | b"WMA " | b"wma " | b"WAV " | b"wav "
            | b"AIFF" | b"aiff" | b"AU  " | b"au  " | b"M4V " | b"m4v " | b"snd " => Kind::Media,
            _ => Kind::File,
        }
    }

    /// The stream type FileMaker stores data of this kind under when a
    /// field takes decoded data named with extension `ext` (script oracle
    /// s34-containers-extensions): the picture and PDF types, the media
    /// type of the extension, `FILE` for any other file.
    pub fn stream_type(self, ext: &str) -> StreamType {
        StreamType(match self {
            Kind::Jpeg => *b"JPEG",
            Kind::Png => *b"PNGf",
            Kind::Gif => *b"GIFf",
            Kind::Bmp => *b"BMPf",
            Kind::Tiff => *b"TIFF",
            Kind::Webp => *b"WEBP",
            Kind::Pdf => *b"PDF ",
            Kind::Mp3 => *b"MP3 ",
            Kind::M4a => *b"M4A ",
            Kind::Media => match ext {
                "mov" | "qt" => *b"MOV ",
                "mp4" => *b"MPG4",
                "m4v" => *b"M4V ",
                "avi" => *b"AVI ",
                "mpg" | "mpeg" => *b"MPEG",
                "wmv" => *b"WMV ",
                "wma" => *b"WMA ",
                "wav" => *b"WAV ",
                "aif" | "aiff" => *b"AIFF",
                "au" => *b"AU  ",
                _ => *b"FILE",
            },
            Kind::File => *b"FILE",
        })
    }

    /// The extension of a file name, in lower case (after the last `.`).
    pub fn extension(name: &str) -> String {
        name.rfind('.')
            .map(|dot| name[dot + 1..].to_ascii_lowercase())
            .unwrap_or_default()
    }

    /// The kind of a decoded file named `name` holding `data`.
    pub fn of_file(name: &str, data: &[u8]) -> Kind {
        let ext = match name.rfind('.') {
            Some(dot) => name[dot + 1..].to_ascii_lowercase(),
            None => return Kind::File,
        };
        let converted = |kind| {
            if picture::read(data).is_some() {
                kind
            } else {
                Kind::File
            }
        };
        match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Kind::Jpeg,
            "png" => Kind::Png,
            "gif" => Kind::Gif,
            "bmp" => converted(Kind::Bmp),
            "tif" | "tiff" => converted(Kind::Tiff),
            "webp" => converted(Kind::Webp),
            "pdf" => Kind::Pdf,
            "mp3" => Kind::Mp3,
            "m4a" => Kind::M4a,
            "wav" | "aif" | "aiff" | "au" | "mov" | "qt" | "mp4" | "m4v" | "avi" | "wmv"
            | "wma" | "mpg" | "mpeg" => Kind::Media,
            _ => Kind::File,
        }
    }
}

/// Container data as the analysis functions see it.
pub(super) struct Media<'a> {
    pub container: &'a Container,
    pub name: String,
    pub kind: Kind,
    /// The main stream's bytes.
    pub data: &'a [u8],
    /// The data was decoded by a calculation (one `FILE` stream): FileMaker
    /// would build its supporting streams when it stores it.
    decoded: bool,
}

/// A value handed to an analysis function.
pub(super) enum Subject<'a> {
    /// Empty text or an empty container: most functions give `?`.
    Empty,
    /// Anything that is not container data, by its text.
    Text(String),
    /// A container the calculation holds only by its reference text: no
    /// data streams.
    Reference(&'a Container),
    Data(Media<'a>),
}

impl<'a> Subject<'a> {
    pub fn of(value: &'a Value, text: impl FnOnce() -> String) -> Subject<'a> {
        match value {
            Value::Container(c) => match c.main_stream() {
                Some((kind, data)) => {
                    let name = c.text.as_str().to_string();
                    // Data typed as stored keeps its type.
                    let decoded = *kind == StreamType::FILE && c.path_type.is_none();
                    let kind = if decoded {
                        Kind::of_file(&name, data)
                    } else {
                        Kind::of_stream(*kind)
                    };
                    Subject::Data(Media {
                        container: c,
                        name,
                        kind,
                        data: c.data(),
                        decoded,
                    })
                }
                None if c.text.is_empty() => Subject::Empty,
                None => Subject::Reference(c),
            },
            v if v.is_empty() => Subject::Empty,
            _ => Subject::Text(text()),
        }
    }
}

impl Media<'_> {
    /// The picture's header, for pictures.
    pub fn picture(&self) -> Option<ImageInfo> {
        if self.kind.is_picture() {
            picture::read(self.data)
        } else {
            None
        }
    }

    /// A stream other than the main one.
    pub fn stream(&self, t: &[u8; 4]) -> Option<&[u8]> {
        self.container
            .streams
            .iter()
            .find(|(kind, _)| &kind.0 == t)
            .map(|(_, data)| data.as_slice())
    }

    /// The picture's stored `SIZE` (u16 width, u16 height).
    pub fn stored_size(&self) -> Option<(u32, u32)> {
        let s = self.stream(b"SIZE")?;
        Some((
            u32::from(u16::from_be_bytes([*s.first()?, *s.get(1)?])),
            u32::from(u16::from_be_bytes([*s.get(2)?, *s.get(3)?])),
        ))
    }

    /// The picture's stored `DPI_` (u16 horizontal, u16 vertical).
    pub fn stored_dpi(&self) -> Option<(u32, u32)> {
        let s = self.stream(b"DPI_")?;
        Some((
            u32::from(u16::from_be_bytes([*s.first()?, *s.get(1)?])),
            u32::from(u16::from_be_bytes([*s.get(2)?, *s.get(3)?])),
        ))
    }

    /// The bytes the data takes in the file (`Internal Size`): every stream
    /// with its 8-byte directory entry (type and size), the file name's
    /// `FNAM` stream included. For decoded data, the streams FileMaker would
    /// store: the data, `FNAM`, `SIZE` for a picture it can read, `DPI_`
    /// when the picture has a resolution, and a 14-byte stream when its
    /// orientation turns or flips it. FileMaker also stores a converted
    /// copy of BMP, TIFF, WebP and PDF data, which is not counted here.
    pub fn internal_size(&self) -> usize {
        // FNAM: a u32 path count, then [u8 len] type [u16 len] name, the
        // texts in SCSU.
        let path = self.container.path_type.unwrap_or(self.kind.path_type());
        let fnam = 4 + 1 + path.len() + 2 + om_scsu::encode_scsu(&self.name).len();
        let mut size = 8 + fnam;
        if !self.decoded {
            return size
                + self
                    .container
                    .streams
                    .iter()
                    .map(|(_, d)| 8 + d.len())
                    .sum::<usize>();
        }
        size += 8 + self.data.len();
        if let Some(info) = self.picture() {
            size += 8 + 4;
            if info.resolution.is_some() {
                size += 8 + 4;
            }
            if matches!(info.orientation(), Some(2..=8)) {
                size += 8 + 14;
            }
        }
        size
    }
}
