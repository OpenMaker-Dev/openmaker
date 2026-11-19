//! Audio attributes of MP3 and M4A data (`GetContainerAttribute`'s
//! `[Audio]` group), as FileMaker Server reads them (oracle):
//!
//! - MP3 needs an ID3v2 tag at the start (a file without one has no audio
//!   attributes) and a frame after it. The duration comes from the
//!   Xing/Info frame count, else from the size at the first frame's bit
//!   rate, in whole seconds (truncated). The bit rate is the first frame's,
//!   which for an `Info` (constant rate) file is the Info frame's own (56
//!   kbps for a 32 kbps file at 22.05 kHz); a `Xing` (variable rate) file
//!   gives its average, truncated, marked `(VBR)`.
//! - M4A: the movie header's duration rounded to seconds (2.5 s is 3) and
//!   the samples' total size over that duration, truncated.

/// The audio attributes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Audio {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// The year text as tagged (`2019`, or a date `2019-05-06`).
    pub year: Option<String>,
    pub track: Option<String>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    /// The cover picture and whether it is a PNG (else a JPEG).
    pub cover: Option<(Vec<u8>, bool)>,
    pub seconds: u64,
    pub kbps: u64,
    pub vbr: bool,
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

fn syncsafe(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at + 4)?;
    Some(b.iter().fold(0, |acc, &x| acc << 7 | usize::from(x & 0x7f)))
}

fn is_png(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG")
}

/// An MP3 file's attributes.
pub(super) fn mp3(data: &[u8]) -> Option<Audio> {
    if !data.starts_with(b"ID3") {
        return None;
    }
    let version = *data.get(3)?;
    let flags = *data.get(5)?;
    let mut end = 10 + syncsafe(data, 6)?;
    if flags & 0x10 != 0 {
        end += 10;
    }
    let mut audio = Audio::default();
    if let Some(tag) = data.get(10..end.min(data.len())) {
        let tag = if flags & 0x80 != 0 && version < 4 {
            resynchronize(tag)
        } else {
            tag.to_vec()
        };
        let start = if flags & 0x40 != 0 && version >= 3 {
            match version {
                3 => 4 + be32(&tag, 0)? as usize,
                _ => syncsafe(&tag, 0)?,
            }
        } else {
            0
        };
        id3_frames(&tag[start.min(tag.len())..], version, &mut audio);
    }
    let start = (end..data.len().saturating_sub(3)).find(|&i| frame(data, i).is_some())?;
    let first = frame(data, start)?;
    let audio_bytes = data.len()
        - start
        - if data.len() >= 128 && data[data.len() - 128..].starts_with(b"TAG") {
            128
        } else {
            0
        };
    let xing = first.xing(data, start);
    let seconds = match xing {
        Some((_, Some(frames), _)) => {
            f64::from(frames) * f64::from(first.samples) / f64::from(first.rate)
        }
        _ => audio_bytes as f64 * 8.0 / f64::from(first.kbps * 1000),
    };
    audio.seconds = seconds as u64;
    match xing {
        Some((true, _, bytes)) if seconds > 0.0 => {
            let bytes = bytes.map_or(audio_bytes as f64, f64::from);
            audio.kbps = (bytes * 8.0 / seconds / 1000.0) as u64;
            audio.vbr = true;
        }
        _ => audio.kbps = u64::from(first.kbps),
    }
    Some(audio)
}

/// Undoes ID3v2.3 unsynchronisation (`FF 00` back to `FF`).
fn resynchronize(tag: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tag.len());
    let mut i = 0;
    while i < tag.len() {
        out.push(tag[i]);
        if tag[i] == 0xff && tag.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    out
}

fn id3_frames(tag: &[u8], version: u8, audio: &mut Audio) {
    let (id_len, header) = if version == 2 { (3, 6) } else { (4, 10) };
    let mut at = 0;
    while at + header <= tag.len() && tag[at] != 0 {
        let id = &tag[at..at + id_len];
        let size = match version {
            2 => {
                usize::from(tag[at + 3]) << 16
                    | usize::from(tag[at + 4]) << 8
                    | usize::from(tag[at + 5])
            }
            3 => be32(tag, at + 4).map_or(0, |s| s as usize),
            _ => syncsafe(tag, at + 4).unwrap_or(0),
        };
        let Some(body) = tag.get(at + header..at + header + size) else {
            break;
        };
        let text = || id3_text(body);
        match id {
            b"TIT2" | b"TT2" => audio.title = text(),
            b"TPE1" | b"TP1" => audio.artist = text(),
            b"TALB" | b"TAL" => audio.album = text(),
            b"TYER" | b"TDRC" | b"TYE" => audio.year = text(),
            b"TRCK" | b"TRK" => audio.track = text(),
            b"TCON" | b"TCO" => audio.genre = text(),
            b"TCOM" | b"TCM" => audio.composer = text(),
            b"APIC" | b"PIC" => {
                if let Some((picture, front)) = id3_picture(body, version == 2) {
                    if audio.cover.is_none() || front {
                        let png = is_png(&picture);
                        audio.cover = Some((picture, png));
                    }
                }
            }
            _ => {}
        }
        at += header + size;
    }
}

/// A text frame's first value.
fn id3_text(body: &[u8]) -> Option<String> {
    let (&encoding, text) = body.split_first()?;
    let (value, _) = id3_string(text, encoding);
    Some(value)
}

/// A string in an ID3 encoding up to its terminator, and the bytes after.
fn id3_string(bytes: &[u8], encoding: u8) -> (String, &[u8]) {
    match encoding {
        1 | 2 => {
            let mut end = 0;
            while end + 1 < bytes.len() && (bytes[end], bytes[end + 1]) != (0, 0) {
                end += 2;
            }
            let units = &bytes[..end.min(bytes.len())];
            let rest = bytes.get(end + 2..).unwrap_or(&[]);
            let (big_endian, units) = match units {
                [0xff, 0xfe, rest @ ..] => (false, rest),
                [0xfe, 0xff, rest @ ..] => (true, rest),
                _ => (encoding == 2, units),
            };
            let units: Vec<u16> = units
                .chunks_exact(2)
                .map(|c| {
                    if big_endian {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            (String::from_utf16_lossy(&units), rest)
        }
        _ => {
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            let rest = bytes.get(end + 1..).unwrap_or(&[]);
            let text = &bytes[..end];
            let value = if encoding == 3 {
                String::from_utf8_lossy(text).into_owned()
            } else {
                text.iter().map(|&b| char::from(b)).collect()
            };
            (value, rest)
        }
    }
}

/// An attached picture's bytes and whether it is the front cover.
fn id3_picture(body: &[u8], v22: bool) -> Option<(Vec<u8>, bool)> {
    let (&encoding, rest) = body.split_first()?;
    let rest = if v22 {
        rest.get(3..)?
    } else {
        let end = rest.iter().position(|&b| b == 0)?;
        &rest[end + 1..]
    };
    let (&kind, rest) = rest.split_first()?;
    let (_, picture) = id3_string(rest, encoding);
    Some((picture.to_vec(), kind == 3))
}

/// An MPEG audio frame header.
struct Frame {
    mpeg1: bool,
    mono: bool,
    kbps: u32,
    rate: u32,
    samples: u32,
}

const KBPS_V1: [[u32; 15]; 3] = [
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
];
const KBPS_V2: [[u32; 15]; 2] = [
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

fn frame(d: &[u8], at: usize) -> Option<Frame> {
    let h = be32(d, at)?;
    if h >> 21 != 0x7ff {
        return None;
    }
    let version = h >> 19 & 3;
    let layer = h >> 17 & 3;
    let bitrate = (h >> 12 & 15) as usize;
    let rate = (h >> 10 & 3) as usize;
    if version == 1 || layer == 0 || bitrate == 0 || bitrate == 15 || rate == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let layer_index = (3 - layer) as usize;
    let kbps = if mpeg1 {
        KBPS_V1[layer_index][bitrate]
    } else {
        KBPS_V2[usize::from(layer_index != 0)][bitrate]
    };
    let rate = [44100, 48000, 32000][rate]
        >> match version {
            3 => 0,
            2 => 1,
            _ => 2,
        };
    let samples = match (layer_index, mpeg1) {
        (0, _) => 384,
        (1, _) | (2, true) => 1152,
        _ => 576,
    };
    Some(Frame {
        mpeg1,
        mono: h >> 6 & 3 == 3,
        kbps,
        rate,
        samples,
    })
}

impl Frame {
    /// A Xing (`true`) or Info (`false`) header in the frame at `at`: its
    /// frame count and byte count when present.
    fn xing(&self, d: &[u8], at: usize) -> Option<(bool, Option<u32>, Option<u32>)> {
        let side = match (self.mpeg1, self.mono) {
            (true, false) => 32,
            (true, true) | (false, false) => 17,
            (false, true) => 9,
        };
        let tag = at + 4 + side;
        let vbr = match d.get(tag..tag + 4)? {
            b"Xing" => true,
            b"Info" => false,
            _ => return None,
        };
        let flags = be32(d, tag + 4)?;
        let mut field = tag + 8;
        let mut take = |bit: u32| {
            (flags & bit != 0).then(|| {
                let v = be32(d, field);
                field += 4;
                v
            })?
        };
        let frames = take(1);
        let bytes = take(2);
        Some((vbr, frames, bytes))
    }
}

/// An M4A (MPEG-4 audio) file's attributes.
pub(super) fn m4a(data: &[u8]) -> Option<Audio> {
    let moov = child(data, b"moov")?;
    let mvhd = child(moov, b"mvhd")?;
    let (scale, duration) = match mvhd.first()? {
        1 => (be32(mvhd, 20)?, be64(mvhd, 24)?),
        _ => (be32(mvhd, 12)?, u64::from(be32(mvhd, 16)?)),
    };
    if scale == 0 {
        return None;
    }
    let seconds = duration as f64 / f64::from(scale);
    let mut audio = Audio {
        seconds: (seconds + 0.5).floor() as u64,
        ..Audio::default()
    };
    let sample_bytes = atoms(moov)
        .filter(|(kind, _)| kind == b"trak")
        .find_map(|(_, trak)| {
            let stbl = child(child(child(trak, b"mdia")?, b"minf")?, b"stbl")?;
            let stsz = child(stbl, b"stsz")?;
            let (size, count) = (be32(stsz, 4)?, be32(stsz, 8)?);
            if size != 0 {
                return Some(u64::from(size) * u64::from(count));
            }
            (0..count as usize)
                .map(|i| be32(stsz, 12 + 4 * i).map(u64::from))
                .sum::<Option<u64>>()
        })
        .or_else(|| child(data, b"mdat").map(|m| m.len() as u64))
        .unwrap_or(0);
    if seconds > 0.0 {
        audio.kbps = (sample_bytes as f64 * 8.0 / seconds / 1000.0) as u64;
    }
    let ilst = child(moov, b"udta")
        .and_then(|udta| child(udta, b"meta"))
        .and_then(|meta| {
            atoms_from(meta.get(4..)?)
                .find(|(k, _)| k == b"ilst")
                .map(|(_, b)| b)
        });
    for (kind, item) in ilst.into_iter().flat_map(atoms) {
        let Some(value) = atoms(item)
            .find(|(k, _)| k == b"data")
            .and_then(|(_, d)| Some((be32(d, 0)? & 0xff_ffff, d.get(8..)?)))
        else {
            continue;
        };
        let text = || Some(String::from_utf8_lossy(value.1).into_owned());
        match &kind {
            b"\xa9nam" => audio.title = text(),
            b"\xa9ART" => audio.artist = text(),
            b"\xa9alb" => audio.album = text(),
            b"\xa9day" => audio.year = text(),
            b"\xa9gen" => audio.genre = text(),
            b"\xa9wrt" => audio.composer = text(),
            b"trkn" => {
                let (n, total) = (be16(value.1, 2), be16(value.1, 4).unwrap_or(0));
                audio.track = n.map(|n| match total {
                    0 => n.to_string(),
                    t => format!("{n}/{t}"),
                });
            }
            b"covr" if audio.cover.is_none() => {
                audio.cover = Some((value.1.to_vec(), value.0 == 14 || is_png(value.1)));
            }
            _ => {}
        }
    }
    Some(audio)
}

/// The atoms of an MP4 box's body, as (type, body).
fn atoms(d: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    atoms_from(d)
}

fn atoms_from(d: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let size = be32(d, at)? as usize;
        let kind: [u8; 4] = d.get(at + 4..at + 8)?.try_into().ok()?;
        let (header, size) = match size {
            0 => (8, d.len() - at),
            1 => (16, usize::try_from(be64(d, at + 8)?).ok()?),
            n => (8, n),
        };
        let body = d.get(at + header..at.checked_add(size)?)?;
        at += size.max(header);
        Some((kind, body))
    })
}

fn child<'a>(d: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    atoms_from(d).find(|(k, _)| k == kind).map(|(_, b)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_headers() {
        // MPEG-2 layer III, 56 kbps, 22.05 kHz (the Info frame of a 32 kbps
        // file).
        let f = frame(&[0xff, 0xf3, 0x70, 0xc0], 0).unwrap();
        assert_eq!((f.kbps, f.rate, f.samples), (56, 22050, 576));
        // MPEG-2.5 layer III, 16 kbps, 8 kHz.
        let f = frame(&[0xff, 0xe3, 0x28, 0xc4], 0).unwrap();
        assert_eq!((f.kbps, f.rate, f.samples), (16, 8000, 576));
        // MPEG-1 layer III, 64 kbps, 44.1 kHz.
        let f = frame(&[0xff, 0xfb, 0x50, 0xc4], 0).unwrap();
        assert_eq!((f.kbps, f.rate, f.samples), (64, 44100, 1152));
        assert!(frame(&[0xff, 0xfb, 0xf0, 0xc4], 0).is_none());
    }

    #[test]
    fn text_frames_in_each_encoding() {
        assert_eq!(id3_text(b"\0Caf\xe9").as_deref(), Some("Café"));
        assert_eq!(id3_text(b"\x03Caf\xc3\xa9\0x").as_deref(), Some("Café"));
        assert_eq!(id3_text(b"\x01\xff\xfeC\0a\0\0\0").as_deref(), Some("Ca"));
        assert_eq!(id3_text(b"\x02\0C\0a").as_deref(), Some("Ca"));
    }
}
