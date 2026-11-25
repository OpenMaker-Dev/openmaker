//! op96 thumbnails (spec `docs/re/containers.md` 6.3): a library member's
//! image scaled to exactly the box the client asks for, encoded as JPEG for a
//! JPEG source and PNG otherwise. Rendered thumbnails are cached, since the
//! client fetches a large one in several chunks and a member's data never
//! changes (member ids are never reused).
//!
//! The image comes from a client (op88), so what a render may allocate is
//! worked out before the image is decoded, in two steps. What reading its
//! header takes ([`header_room`]) comes from the data alone, after the
//! data's own claims a decoder would reserve memory for are checked against
//! its length. What the render takes ([`plan`]) comes from the header: the
//! decoded image, the decoder's own buffers, the scaled image and its
//! encoding. The renders in progress together hold at most [`BUDGET`]:
//! each step waits for room before it starts, and one above it is refused.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, Cursor, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::engine::{ReadError, View, NO_LIMIT};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{
    ColorType, DynamicImage, ImageBuffer, ImageDecoder, ImageFormat, ImageReader, Limits, Pixel,
};

/// The largest box side served; FileMaker Pro asks for display sizes.
pub const MAX_SIDE: u32 = 4096;

/// The image streams the server can scale, in order of preference (a member
/// holding both `PNGf` and `JPEG` is shown from `PNGf`, 6.2).
const MASTERS: [(&[u8; 4], ImageFormat); 6] = [
    (b"PNGf", ImageFormat::Png),
    (b"JPEG", ImageFormat::Jpeg),
    (b"GIFf", ImageFormat::Gif),
    (b"BMPf", ImageFormat::Bmp),
    (b"TIFF", ImageFormat::Tiff),
    (b"WEBP", ImageFormat::WebP),
];

/// Decoding limits: no source image may claim more than this.
const MAX_SOURCE_SIDE: u32 = 16_384;
const MAX_DECODE_BYTES: u64 = 512 << 20;

/// The most the renders in progress may hold together, and so the most
/// one render may plan to hold.
const BUDGET: u64 = 1 << 30;
/// Room for a decoder's own small buffers while it decodes (rows, tables,
/// TIFF's strips beyond the image) and the encoders' rows.
const SLACK: u64 = 16 << 20;
/// Room for what reading a header takes besides what [`header_room`]
/// counts from the data: a decoder's state and tables, a row, a reader's
/// buffer.
const HEADER_FLOOR: u64 = 1 << 20;

/// The quality of JPEG thumbnails.
const JPEG_QUALITY: u8 = 85;

/// A rendered thumbnail.
#[derive(Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The master image stream of the library member at `member` in the
/// version `view` reads: its format and bytes. Each stream is read whole,
/// in [`MASTERS`]' order, until one assembles.
pub fn master(
    view: &dyn View,
    member: &[&[u8]],
) -> Result<Option<(ImageFormat, Vec<u8>)>, ReadError> {
    for (kind, format) in MASTERS {
        let path: Vec<&[u8]> = member.iter().copied().chain([kind.as_slice()]).collect();
        if let Some(bytes) = view
            .subtree(&path, NO_LIMIT)?
            .and_then(|n| n.assembled().map(Cow::into_owned))
        {
            return Ok(Some((format, bytes)));
        }
    }
    Ok(None)
}

/// Decode `bytes` and scale them to `width` x `height`, within the memory
/// [`plan`] works out for them.
pub fn render(
    format: ImageFormat,
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<Thumbnail, String> {
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
        return Err(format!("no thumbnail is made at {width} x {height}"));
    }
    let header = header_room(format, bytes)?;
    let plan = {
        let _room = Reservation::take(header);
        let decoder = decoder(format, bytes)?;
        plan(
            format,
            decoder.color_type(),
            decoder.dimensions(),
            (width, height),
            bytes,
            header,
        )?
    };
    // The header is read again once there is room, so that a render
    // waiting for room holds nothing beyond the data.
    let _room = Reservation::take(plan.peak);
    let mut decoder = decoder(format, bytes)?;
    decoder
        .set_limits(limits(plan.decoder))
        .map_err(|e| e.to_string())?;
    let image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let scaled = resample(&image, width, height).ok_or("an image of this kind is not scaled")?;
    drop(image);
    let mut out = Capped::new(plan.encoded);
    if format == ImageFormat::Jpeg {
        scaled
            .into_rgb8()
            .write_with_encoder(JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY))
    } else {
        scaled.write_with_encoder(PngEncoder::new(&mut out))
    }
    .map_err(|e| e.to_string())?;
    Ok(Thumbnail {
        bytes: out.bytes,
        width,
        height,
    })
}

/// A decoder for `bytes` that has read the image's header.
fn decoder(format: ImageFormat, bytes: &[u8]) -> Result<impl ImageDecoder + '_, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    // The PNG decoder takes the limit for its own buffers when it is made;
    // the others take theirs once the plan is known.
    reader.limits(limits(png_limit(bytes)));
    reader.into_decoder().map_err(|e| e.to_string())
}

fn limits(max_alloc: u64) -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(max_alloc);
    limits
}

/// What a render will hold, in bytes.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    /// The most at any moment: the largest of decoding (the image and the
    /// decoder's buffers), scaling (the image, the scaled image and the
    /// scaler's rows) and encoding (the scaled image and the encoders'
    /// buffers; the source is freed by then).
    peak: u64,
    /// The limit given to the decoders that take one (GIF and TIFF; PNG's
    /// is [`png_limit`]).
    decoder: u64,
    /// The largest encoded thumbnail accepted.
    encoded: u64,
}

/// What reading the header of `bytes` as `format` holds, from the data
/// alone; an error for a TIFF that claims values its data does not hold,
/// or for more than [`BUDGET`].
///
/// The figures are read from the decoders' sources (the versions in
/// [`plan`]) and checked against measurements of hostile files:
/// - JPEG: the decoder copies the data and keeps its metadata segments
///   (ICC, EXIF, XMP), and joins extended XMP into one more copy. A segment
///   is at most 64 KiB and its data must be present.
/// - PNG: see [`png_limit`]; an ICC profile is decompressed beside it into
///   a vector that grows by doubling up to that limit, so up to 3 times it
///   while it moves.
/// - GIF: XMP and ICC extensions are kept as they arrive, in vectors that
///   grow by doubling (up to 3 times the data while they move).
/// - WebP: only the known chunk kinds are indexed, one entry each, and only
///   the 6-byte ANIM chunk is read.
/// - BMP: fixed headers and a palette of at most 256 colours.
/// - TIFF: see [`tiff_tags`].
fn header_room(format: ImageFormat, bytes: &[u8]) -> Result<u64, String> {
    let input = bytes.len() as u64;
    let room = match format {
        ImageFormat::Png => 4 * png_limit(bytes),
        ImageFormat::Jpeg => 3 * input + HEADER_FLOOR,
        ImageFormat::Tiff => tiff_tags(bytes)?.held + HEADER_FLOOR,
        ImageFormat::Gif => 3 * input + HEADER_FLOOR,
        _ => HEADER_FLOOR,
    };
    if room > BUDGET {
        return Err(format!(
            "reading the header of {input} bytes would take {} MiB",
            room >> 20
        ));
    }
    Ok(room)
}

/// What rendering an image of `color` and `source` size as `format` into
/// `target` holds, from the image's header and what reading it holds
/// (`header`, from [`header_room`]); an error for an image the server does
/// not render or a plan above [`BUDGET`].
///
/// Each figure is an upper bound on what the `image` crate, its decoders
/// and its encoders allocate, read from their sources (image 0.25, png
/// 0.18, tiff 0.11, zune-jpeg 0.5, image-webp 0.2, gif 0.14). The decoders
/// that take a memory limit are given the share planned for them (PNG for
/// its own buffers, GIF and TIFF); JPEG's and WebP's are worked out here.
fn plan(
    format: ImageFormat,
    color: ColorType,
    (source_width, source_height): (u32, u32),
    (width, height): (u32, u32),
    bytes: &[u8],
    header: u64,
) -> Result<Plan, String> {
    // The PNG encoder writes neither, so they never made a thumbnail.
    if matches!(color, ColorType::Rgb32F | ColorType::Rgba32F) {
        return Err(format!("{color:?} images are not scaled"));
    }
    let pixel = u64::from(color.bytes_per_pixel());
    let (sw, sh) = (u64::from(source_width), u64::from(source_height));
    let (tw, th) = (u64::from(width), u64::from(height));
    let decoded = sw * sh * pixel;
    if decoded > MAX_DECODE_BYTES {
        return Err(format!(
            "a {source_width} x {source_height} image is too large"
        ));
    }
    let input = bytes.len() as u64;
    let decoder = match format {
        ImageFormat::Png => png_limit(bytes),
        // The buffer a TIFF is read into (4 bytes for every 3 when CMYK
        // becomes RGB) and the tags it keeps, as large as the data (which
        // also lets a compressed strip or tile that large pass the
        // decoder's check; it is streamed, unless it is a JPEG); the
        // frame GIF composes, no larger than the image.
        ImageFormat::Tiff | ImageFormat::Gif => decoded * 4 / 3 + input + SLACK,
        // BMP keeps an ICC profile, as large as the data.
        _ => input + SLACK,
    };
    // What the header read holds stays held while the image is decoded,
    // beside what decoding takes.
    let working = header
        + match format {
            // PNG's limit, within the header's room, covers both.
            ImageFormat::Png => 0,
            ImageFormat::Gif => decoder,
            ImageFormat::Tiff => decoder + tiff_jpeg(bytes, sw, sh),
            ImageFormat::Jpeg => jpeg_coefficients(bytes, sw, sh, pixel) + SLACK,
            // VP8 planes (1.5 bytes a pixel), the alpha plane, a lossless
            // image's 4-byte pixels and its transforms, an animation's
            // canvas and frame (4 each), and a copy of the chunk decoded.
            ImageFormat::WebP => 12 * (sw + 16) * (sh + 16) + input + SLACK,
            _ => decoder,
        };
    let scaled = tw * th * pixel;
    // The scaler holds one source row of floats and the weights.
    let rows = 64 * (sw + sh + tw);
    // The encoded thumbnail grows by doubling up to its cap: twice the cap
    // while it moves.
    let (encoded, encoding) = if format == ImageFormat::Jpeg {
        // A JPEG at quality 85 is far smaller than its pixels; one larger
        // than them is refused. A grey image is converted to RGB first.
        let encoded = tw * th * 3 + SLACK;
        let rgb = if pixel == 1 { tw * th * 3 } else { 0 };
        (encoded, rgb + 2 * encoded)
    } else {
        // Deflate stores what it cannot compress: the filtered rows, 5
        // bytes per 64 KiB block and the chunks around them. The PNG
        // encoder copies 16-bit images to swap their byte order and keeps
        // the whole compressed stream (a vector, so up to twice as large)
        // before writing it out.
        let encoded = scaled + th + scaled / 4096 + SLACK;
        let sixteen_bit = color.bytes_per_pixel() == 2 * color.channel_count();
        let swapped = if sixteen_bit { scaled } else { 0 };
        (encoded, swapped + 4 * encoded)
    };
    // The source is freed before encoding.
    let peak = (decoded + working)
        .max(decoded + scaled + rows)
        .max(scaled + encoding);
    if peak > BUDGET {
        return Err(format!(
            "rendering a {source_width} x {source_height} {color:?} image at {width} x {height} \
             would take {} MiB",
            peak >> 20
        ));
    }
    Ok(Plan {
        peak,
        decoder,
        encoded,
    })
}

/// The limit for the PNG decoder's own buffers, given when it is made, so
/// that it covers reading the header too. The decoder counts against it
/// every ancillary chunk it keeps (text, ICC, EXIF): its data, read into a
/// vector that grows by doubling, and the copy it keeps, 3 times the chunk
/// in all; and a row, within [`HEADER_FLOOR`]. It decompresses an ICC
/// profile up to what is left of the limit; a larger one is dropped, and
/// renders do not use it. The chunks are counted from the data, so a
/// photograph whose data is almost all pixels gets a small limit.
fn png_limit(bytes: &[u8]) -> u64 {
    3 * png_kept(bytes) + HEADER_FLOOR
}

/// The total size of the ancillary chunks the PNG decoder keeps (text, ICC
/// and EXIF), as far as the data holds them, from the chunk headers alone.
fn png_kept(bytes: &[u8]) -> u64 {
    // After the signature, each chunk is a length, a kind, the data and a
    // checksum.
    let mut at = 8usize;
    let mut kept = 0u64;
    while let Some(header) = bytes.get(at..).and_then(|rest| rest.first_chunk::<8>()) {
        let (len, kind) = header.split_at(4);
        let len = u64::from(u32::from_be_bytes(len.try_into().expect("4 bytes")));
        if matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"iCCP" | b"eXIf") {
            kept += len.min((bytes.len() - at - 8) as u64);
        }
        if kind == b"IEND" {
            break;
        }
        let Some(next) = usize::try_from(len)
            .ok()
            .and_then(|len| at.checked_add(12)?.checked_add(len))
        else {
            break;
        };
        at = next;
    }
    kept
}

/// What zune-jpeg (the `image` crate's JPEG decoder) holds besides the
/// image: the 2-byte DCT coefficients of every component for the whole
/// image when the JPEG is progressive or its first scan lacks a component,
/// otherwise one row of blocks. Blocks cover up to 32 x 32 pixels; a JPEG
/// the decoder accepts has at most 4 components.
fn jpeg_coefficients(bytes: &[u8], width: u64, height: u64, pixel: u64) -> u64 {
    let (whole, components) = jpeg_layout(bytes).unwrap_or((true, if pixel == 1 { 1 } else { 4 }));
    let rows = if whole { height + 32 } else { 64 };
    u64::from(components.min(4)) * 2 * (width + 32) * rows
}

/// What the tiff crate holds besides its buffer while it decodes a TIFF
/// whose strips or tiles are JPEG-compressed (or whose compression cannot
/// be read): a strip or tile with its tables, read into a vector that grows
/// by doubling (up to 3 times the data while it moves), the metadata
/// zune-jpeg keeps from it, and the strip decoded into pixels of its own
/// (up to 4 bytes each) from coefficients that may be held for all of it
/// (see [`jpeg_coefficients`]).
fn tiff_jpeg(bytes: &[u8], width: u64, height: u64) -> u64 {
    if tiff_tags(bytes).is_ok_and(|tags| tags.compression.is_some_and(|c| !matches!(c, 6 | 7))) {
        return 0;
    }
    4 * bytes.len() as u64 + (4 + 8) * (width + 32) * (height + 32)
}

/// What the tiff crate makes of a TIFF's first directory before and while
/// decoding it.
#[derive(Debug, PartialEq, Eq)]
struct TiffTags {
    /// The first image's compression (tag 259).
    compression: Option<u16>,
    /// What it holds: every entry, in a map, and the values of the tags it
    /// reads, as 32-byte values while it reads them and then as numbers.
    held: u64,
}

/// The tags whose values the tiff crate (`Image::from_reader`) and the
/// `image` crate (`TiffDecoder::new`) read to make and use a decoder:
/// size, samples, compression, layout, strips, tiles and JPEG tables.
/// Others (ICC, XMP, orientation) are read only on request, which
/// rendering never makes.
const TIFF_READ_TAGS: [u64; 19] = [
    256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 317, 322, 323, 324, 325, 338, 339, 347, 530,
];

/// The first directory of a TIFF, checked as far as the tiff crate will
/// trust it: it reserves room for a tag's values from the count the tag
/// claims before it reads them (so a 122-byte file claiming 8 million strip
/// offsets made it reserve 244 MiB), so a tag it reads must have its values
/// in the data. An error for a TIFF whose directory or such a tag's values
/// lie beyond the data; the tiff crate would fail to read it too.
fn tiff_tags(bytes: &[u8]) -> Result<TiffTags, String> {
    let unreadable = || "a TIFF whose first directory is not in its data".to_string();
    let big_endian = match bytes.get(..2) {
        Some(b"II") => false,
        Some(b"MM") => true,
        _ => return Err(unreadable()),
    };
    let read = |at: u64, len: u64| -> Option<u64> {
        let at = usize::try_from(at).ok()?;
        let field = bytes.get(at..at.checked_add(usize::try_from(len).ok()?)?)?;
        let next = |n: u64, &b: &u8| n << 8 | u64::from(b);
        Some(if big_endian {
            field.iter().fold(0, next)
        } else {
            field.iter().rev().fold(0, next)
        })
    };
    // A classic TIFF has 4-byte offsets, a 2-byte entry count and 12-byte
    // entries; a BigTIFF 8, 8 and 20. An entry is a tag, a type, a count
    // and the values themselves when they fit in the offset's place.
    let (directory, count_len, entry_len, offset_len) = match read(2, 2) {
        Some(42) => (read(4, 4), 2, 12, 4),
        Some(43) if read(4, 2) == Some(8) && read(6, 2) == Some(0) => (read(8, 8), 8, 20, 8),
        _ => return Err(unreadable()),
    };
    let directory = directory.ok_or_else(unreadable)?;
    let entries = read(directory, count_len).ok_or_else(unreadable)?;
    let first = directory + count_len;
    let end = entries
        .checked_mul(entry_len)
        .and_then(|len| first.checked_add(len))
        .filter(|&end| end <= bytes.len() as u64)
        .ok_or_else(unreadable)?;
    // Each entry in the map: the tag, its type, count and offset, and the
    // map's own share.
    let mut held = entries * 64;
    let mut compression = None;
    for entry in (first..end).step_by(entry_len as usize) {
        let (tag, kind) = (read(entry, 2), read(entry + 2, 2));
        let (Some(tag), Some(kind)) = (tag, kind) else {
            return Err(unreadable());
        };
        let count = read(entry + 4, offset_len).ok_or_else(unreadable)?;
        let value_at = entry + 4 + offset_len;
        let size = match kind {
            1 | 2 | 6 | 7 => 1,              // BYTE, ASCII, SBYTE, UNDEFINED
            3 | 8 => 2,                      // SHORT, SSHORT
            4 | 9 | 11 | 13 => 4,            // LONG, SLONG, FLOAT, IFD
            5 | 10 | 12 | 16 | 17 | 18 => 8, // RATIONAL, SRATIONAL, DOUBLE, LONG8, SLONG8, IFD8
            // The tiff crate skips an entry of a type it does not know.
            _ => continue,
        };
        if !TIFF_READ_TAGS.contains(&tag) {
            continue;
        }
        let len = count.checked_mul(size).ok_or_else(unreadable)?;
        if len > offset_len {
            let offset = read(value_at, offset_len).ok_or_else(unreadable)?;
            if !offset
                .checked_add(len)
                .is_some_and(|end| end <= bytes.len() as u64)
            {
                return Err(format!(
                    "a TIFF whose tag {tag} claims {count} values its data does not hold"
                ));
            }
        }
        held += count * 40;
        if tag == 259 && len <= offset_len {
            compression = u16::try_from(read(value_at, size).ok_or_else(unreadable)?).ok();
        }
    }
    Ok(TiffTags { compression, held })
}

/// From a JPEG's headers, whether it is decoded from coefficients held for
/// the whole image (it is progressive, or its first scan lacks a
/// component), and its component count. `None` if the headers before the
/// first scan cannot be read.
fn jpeg_layout(bytes: &[u8]) -> Option<(bool, u8)> {
    let mut rest = bytes.strip_prefix(&[0xff, 0xd8])?;
    let mut frame = None;
    loop {
        // A marker is 0xff (repeated as fill) and a code.
        let fill = rest.iter().take_while(|&&b| b == 0xff).count();
        if fill == 0 {
            return None;
        }
        let (&marker, after) = rest[fill..].split_first()?;
        rest = after;
        match marker {
            // Markers without a segment.
            0x01 | 0xd0..=0xd7 => continue,
            0xd8 | 0xd9 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes(*rest.first_chunk()?));
        let segment = rest.get(2..len)?;
        rest = &rest[len..];
        match marker {
            // Start of frame: precision, height, width, components.
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                let progressive = matches!(marker, 0xc2 | 0xc6 | 0xca | 0xce);
                frame = Some((progressive, *segment.get(5)?));
            }
            // Start of scan: its component count first.
            0xda => {
                let (progressive, components) = frame?;
                return Some((progressive || *segment.first()? != components, components));
            }
            _ => {}
        }
    }
}

/// Memory the renders in progress have reserved.
static RESERVED: Mutex<u64> = Mutex::new(0);
static RELEASED: Condvar = Condvar::new();
/// Held by the render waiting for room, so that renders arriving after it
/// wait behind it instead of taking the room it waits for.
static QUEUE: Mutex<()> = Mutex::new(());

/// A render's share of [`BUDGET`], returned when dropped.
struct Reservation(u64);

impl Reservation {
    /// Wait until `bytes` fit beside the other renders' reservations. The
    /// caller holds no other lock: renders run outside the store and the
    /// thumbnail cache.
    fn take(bytes: u64) -> Self {
        let _turn = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        let mut reserved = RESERVED.lock().unwrap_or_else(PoisonError::into_inner);
        while *reserved + bytes > BUDGET {
            reserved = RELEASED
                .wait(reserved)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *reserved += bytes;
        Reservation(bytes)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        *RESERVED.lock().unwrap_or_else(PoisonError::into_inner) -= self.0;
        RELEASED.notify_all();
    }
}

/// A buffer that refuses to grow past the size planned for it.
struct Capped {
    bytes: Vec<u8>,
    cap: usize,
}

impl Capped {
    fn new(cap: u64) -> Self {
        Self {
            bytes: Vec::new(),
            cap: usize::try_from(cap).unwrap_or(usize::MAX),
        }
    }
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = self.bytes.len();
        if buf.len() > self.cap - len {
            return Err(io::Error::other("the thumbnail is larger than planned"));
        }
        if self.bytes.capacity() - len < buf.len() {
            let grown = (self.bytes.capacity() * 2).clamp(len + buf.len(), self.cap);
            self.bytes.reserve_exact(grown - len);
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `image` scaled to `width` x `height` with the Catmull-Rom filter.
///
/// The result is pixel for pixel what `imageops::resize` with
/// `FilterType::CatmullRom` returns: the same weights and the same
/// arithmetic in the same order. That function scales the columns of the
/// whole image first, into floats for 4 channels whatever the image holds,
/// and so holds 16 bytes for each pixel of the source's width by the
/// target's height (a gigabyte for a 16384 x 1 source asked for at 4096 x
/// 4096). This one scales one output row at a time, holding one source row
/// of floats. `None` for floating-point images.
fn resample(image: &DynamicImage, width: u32, height: u32) -> Option<DynamicImage> {
    Some(match image {
        DynamicImage::ImageLuma8(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageLumaA8(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageRgb8(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageRgba8(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageLuma16(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageLumaA16(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageRgb16(i) => resample_buffer(i, width, height).into(),
        DynamicImage::ImageRgba16(i) => resample_buffer(i, width, height).into(),
        _ => return None,
    })
}

/// An 8- or 16-bit channel value.
trait Channel: image::Primitive {
    const MAX: f32;
    fn to_f32(self) -> f32;
    /// From a value already clamped to `0..=MAX`, rounded half away from
    /// zero.
    fn from_f32(value: f32) -> Self;
}

impl Channel for u8 {
    const MAX: f32 = 255.0;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32) -> Self {
        value.round() as u8
    }
}

impl Channel for u16 {
    const MAX: f32 = 65535.0;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32) -> Self {
        value.round() as u16
    }
}

fn resample_buffer<P, S>(
    source: &ImageBuffer<P, Vec<S>>,
    width: u32,
    height: u32,
) -> ImageBuffer<P, Vec<S>>
where
    P: Pixel<Subpixel = S>,
    S: Channel,
{
    let (source_width, source_height) = source.dimensions();
    // As `imageops::resize`: nothing to sample from gives a blank image,
    // the same size a copy.
    if source_width == 0 || source_height == 0 {
        return ImageBuffer::new(width, height);
    }
    if (source_width, source_height) == (width, height) {
        return source.clone();
    }
    let channels = usize::from(P::CHANNEL_COUNT);
    let source_stride = source_width as usize * channels;
    let stride = width as usize * channels;
    let columns = Taps::new(source_width, width);
    let mut weights = Vec::new();
    let mut row = vec![0f32; source_stride];
    let mut out = ImageBuffer::<P, Vec<S>>::new(width, height);
    let source = source.as_raw();
    for (y, out_row) in (0..height).zip(out.chunks_exact_mut(stride)) {
        // The source rows this output row draws on, weighted; summed per
        // value in the order of the rows, as `imageops::resize` does.
        let first = taps(source_height, height, y, &mut weights);
        row.fill(0.0);
        for (i, &weight) in weights.iter().enumerate() {
            let line = &source[(first + i) * source_stride..][..source_stride];
            for (sum, &value) in row.iter_mut().zip(line) {
                *sum += value.to_f32() * weight;
            }
        }
        for (x, out_pixel) in out_row.chunks_exact_mut(channels).enumerate() {
            let (first, weights) = columns.get(x);
            for (c, value) in out_pixel.iter_mut().enumerate() {
                let mut sum = 0f32;
                for (i, &weight) in weights.iter().enumerate() {
                    sum += row[(first + i) * channels + c] * weight;
                }
                *value = S::from_f32(clamp(sum, 0.0, S::MAX));
            }
        }
    }
    out
}

/// The weights of every output column, computed once.
struct Taps {
    /// Per output pixel: its first source pixel, and where its weights
    /// start in `weights`.
    starts: Vec<(usize, usize)>,
    weights: Vec<f32>,
}

impl Taps {
    fn new(source: u32, target: u32) -> Self {
        let mut starts = Vec::with_capacity(target as usize);
        let mut weights = Vec::new();
        let mut these = Vec::new();
        for out in 0..target {
            let first = taps(source, target, out, &mut these);
            starts.push((first, weights.len()));
            weights.extend_from_slice(&these);
        }
        Self { starts, weights }
    }

    fn get(&self, out: usize) -> (usize, &[f32]) {
        let (first, start) = self.starts[out];
        let end = self
            .starts
            .get(out + 1)
            .map_or(self.weights.len(), |&(_, next)| next);
        (first, &self.weights[start..end])
    }
}

/// The source pixels output pixel `out` of `target` draws on along one
/// axis of `source` pixels: the first one's index, and their normalized
/// Catmull-Rom weights in `weights`. Computed as `imageops::resize` does,
/// operation for operation.
fn taps(source: u32, target: u32, out: u32, weights: &mut Vec<f32>) -> usize {
    const SUPPORT: f32 = 2.0;
    let ratio = source as f32 / target as f32;
    let scale = if ratio < 1.0 { 1.0 } else { ratio };
    let support = SUPPORT * scale;
    let centre = (out as f32 + 0.5) * ratio;
    let left = clamp((centre - support).floor() as i64, 0, i64::from(source) - 1);
    let right = clamp(
        (centre + support).ceil() as i64,
        left + 1,
        i64::from(source),
    );
    let centre = centre - 0.5;
    weights.clear();
    let mut sum = 0.0;
    for i in left..right {
        let weight = catmull_rom((i as f32 - centre) / scale);
        weights.push(weight);
        sum += weight;
    }
    for weight in weights.iter_mut() {
        *weight /= sum;
    }
    left as usize
}

/// The Catmull-Rom cubic (B = 0, C = 0.5).
fn catmull_rom(x: f32) -> f32 {
    let a = x.abs();
    let k = if a < 1.0 {
        9.0 * a.powi(3) + -15.0 * a.powi(2) + 6.0
    } else if a < 2.0 {
        -3.0 * a.powi(3) + 15.0 * a.powi(2) + -24.0 * a + 12.0
    } else {
        0.0
    };
    k / 6.0
}

fn clamp<N: PartialOrd>(value: N, min: N, max: N) -> N {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Which thumbnail: the library, the member, and the box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub library: Vec<Vec<u8>>,
    pub member: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Recently rendered thumbnails of one file, oldest first.
#[derive(Debug, Default)]
pub struct Cache {
    entries: VecDeque<(Key, Arc<Thumbnail>)>,
    bytes: usize,
}

/// How many thumbnails, and how many bytes of them, a file keeps.
const CACHE_ENTRIES: usize = 64;
const CACHE_BYTES: usize = 64 << 20;

impl Cache {
    pub fn get(&self, key: &Key) -> Option<Arc<Thumbnail>> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, t)| t.clone())
    }

    /// Forget every thumbnail of `library`'s members: the library went
    /// with its table (op86 DeleteTable).
    pub fn forget_library(&mut self, library: &[Vec<u8>]) {
        self.entries.retain(|(key, _)| key.library != library);
        self.bytes = self.entries.iter().map(|(_, t)| t.bytes.len()).sum();
    }

    pub fn insert(&mut self, key: Key, thumbnail: Arc<Thumbnail>) {
        if self.get(&key).is_some() {
            return;
        }
        self.bytes += thumbnail.bytes.len();
        self.entries.push_back((key, thumbnail));
        while self.entries.len() > CACHE_ENTRIES
            || (self.bytes > CACHE_BYTES && self.entries.len() > 1)
        {
            let (_, old) = self.entries.pop_front().expect("nonempty");
            self.bytes -= old.bytes.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgb, RgbImage};
    use om_hbam::OwnedNode;

    fn encoded(format: ImageFormat) -> Vec<u8> {
        let image = RgbImage::from_fn(48, 48, |x, y| Rgb([x as u8 * 5, y as u8 * 5, 128]));
        let mut out = Vec::new();
        image.write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    #[test]
    fn a_deleted_library_forgets_its_thumbnails() {
        let mut cache = Cache::default();
        let key = |table: u8| Key {
            library: vec![vec![0x80, table], vec![0x1f]],
            member: vec![1],
            width: 2,
            height: 2,
        };
        let thumbnail = || {
            Arc::new(Thumbnail {
                bytes: vec![0; 10],
                width: 2,
                height: 2,
            })
        };
        cache.insert(key(2), thumbnail());
        cache.insert(key(3), thumbnail());
        cache.forget_library(&key(2).library);
        assert!(cache.get(&key(2)).is_none());
        assert!(cache.get(&key(3)).is_some());
        assert_eq!(cache.bytes, 10);
    }

    #[test]
    fn scales_to_exactly_the_box() {
        // live:c7: a 48 x 48 PNG asked for at 64 x 32 comes back 64 x 32 PNG.
        let thumb = render(ImageFormat::Png, &encoded(ImageFormat::Png), 64, 32).unwrap();
        assert_eq!((thumb.width, thumb.height), (64, 32));
        let decoded = image::load_from_memory_with_format(&thumb.bytes, ImageFormat::Png).unwrap();
        assert_eq!(decoded.dimensions(), (64, 32));

        // A JPEG source gets a JPEG thumbnail.
        let thumb = render(ImageFormat::Jpeg, &encoded(ImageFormat::Jpeg), 20, 30).unwrap();
        assert_eq!(&thumb.bytes[..2], [0xff, 0xd8]);
        let decoded = image::load_from_memory_with_format(&thumb.bytes, ImageFormat::Jpeg).unwrap();
        assert_eq!(decoded.dimensions(), (20, 30));

        assert!(render(ImageFormat::Png, b"not a picture", 10, 10).is_err());
    }

    #[test]
    fn picks_the_master_stream() {
        let png = encoded(ImageFormat::Png);
        let member = OwnedNode::list(
            vec![1],
            vec![
                OwnedNode::leaf(b"FNAM".to_vec(), b"x".to_vec()),
                OwnedNode::leaf(b"JPEG".to_vec(), b"jpeg bytes".to_vec()),
                OwnedNode::leaf(b"PNGf".to_vec(), png.clone()),
            ],
        );
        let file = OwnedNode::list(
            vec![2],
            vec![OwnedNode::leaf(b"FILE".to_vec(), b"x".to_vec())],
        );
        let root = OwnedNode::list(Vec::new(), vec![member, file]);
        let view = crate::engine::TreeView::new(&root, 0);
        assert_eq!(master(&view, &[&[1]]), Ok(Some((ImageFormat::Png, png))));
        assert_eq!(master(&view, &[&[2]]), Ok(None));
    }

    #[test]
    fn the_cache_is_bounded() {
        let mut cache = Cache::default();
        let key = |n: u32| Key {
            library: vec![vec![0x1f]],
            member: vec![1],
            width: n,
            height: n,
        };
        for n in 0..100 {
            let thumbnail = Thumbnail {
                bytes: vec![0; 10],
                width: n,
                height: n,
            };
            cache.insert(key(n), Arc::new(thumbnail));
        }
        assert!(cache.get(&key(0)).is_none());
        assert_eq!(cache.get(&key(99)).unwrap().width, 99);
        assert_eq!(cache.entries.len(), CACHE_ENTRIES);
        assert_eq!(cache.bytes, CACHE_ENTRIES * 10);
    }

    #[test]
    fn scales_as_imageops_resize_does() {
        // Thumbnails keep their pixels: the row-at-a-time scaler gives what
        // `imageops::resize` with Catmull-Rom gives, for every pixel type
        // it renders, scaling down, up, and one way on each axis.
        use image::imageops::FilterType;
        use image::{ImageBuffer, Rgba};
        for (sw, sh, tw, th) in [
            (48, 48, 64, 32),
            (300, 7, 17, 90),
            (5, 400, 3, 3),
            (1, 1, 9, 4),
            (97, 61, 97, 30),
        ] {
            let base = DynamicImage::from(ImageBuffer::from_fn(sw, sh, |x, y| {
                let v = |k: u32| ((x * 7919 + y * 104_729 + k * 7) % 65_536) as u16;
                Rgba([v(0), v(1) ^ 0x5a5a, v(2).rotate_left(5), v(3) | 0x0f00])
            }));
            for source in [
                base.to_luma8().into(),
                base.to_luma_alpha8().into(),
                base.to_rgb8().into(),
                base.to_rgba8().into(),
                base.to_luma16().into(),
                base.to_luma_alpha16().into(),
                base.to_rgb16().into(),
                base.clone(),
            ] {
                let expected = source.resize_exact(tw, th, FilterType::CatmullRom);
                let scaled = resample(&source, tw, th).unwrap();
                assert_eq!(scaled.color(), expected.color());
                assert!(
                    scaled.as_bytes() == expected.as_bytes(),
                    "{:?} {sw} x {sh} at {tw} x {th}",
                    source.color()
                );
            }
        }
    }

    #[test]
    fn plans_what_a_render_holds() {
        // A 198-byte PNG, 16384 x 1, asked for at 4096 x 4096 took 1 GiB
        // (#304); it now plans a sixth of that and holds less.
        let mut sliver = Vec::new();
        image::GrayImage::new(16_384, 1)
            .write_to(&mut Cursor::new(&mut sliver), ImageFormat::Png)
            .unwrap();
        let sliver = plan(
            ImageFormat::Png,
            ColorType::L8,
            (16_384, 1),
            (4096, 4096),
            &sliver,
            header_room(ImageFormat::Png, &sliver).unwrap(),
        )
        .unwrap();
        assert!(sliver.peak < 160 << 20, "{sliver:?}");

        // Whether a JPEG keeps its coefficients for the whole image is read
        // from its headers: not for a baseline JPEG whose first scan holds
        // every component, as the encoder writes it...
        let jpeg = encoded(ImageFormat::Jpeg);
        assert_eq!(jpeg_layout(&jpeg), Some((false, 3)));
        // ...but for a progressive one, or one whose first scan holds fewer.
        let sof = jpeg.windows(2).position(|m| m == [0xff, 0xc0]).unwrap();
        let mut progressive = jpeg.clone();
        progressive[sof + 1] = 0xc2;
        assert_eq!(jpeg_layout(&progressive), Some((true, 3)));
        let sos = jpeg.windows(2).position(|m| m == [0xff, 0xda]).unwrap();
        let mut separate = jpeg.clone();
        separate[sos + 4] = 1;
        assert_eq!(jpeg_layout(&separate), Some((true, 3)));
        assert_eq!(jpeg_layout(&jpeg[..sof]), None);

        // A TIFF's compression, which says whether its strips are JPEGs,
        // is read from its first directory (the encoder writes 1, none).
        let mut tiff = Vec::new();
        image::GrayImage::new(4, 4)
            .write_to(&mut Cursor::new(&mut tiff), ImageFormat::Tiff)
            .unwrap();
        assert_eq!(tiff_tags(&tiff).map(|tags| tags.compression), Ok(Some(1)));
        assert!(tiff_tags(&tiff[..12]).is_err());

        // A tag the tiff crate reads may not claim values its data does not
        // hold, since the crate reserves room for them before reading: a
        // 122-byte TIFF whose strip tables claim 8 million entries made it
        // reserve 244 MiB while reading the header (#375).
        let mut strips = b"II*\0\x08\0\0\0\x09\0".to_vec();
        for (tag, kind, count, value) in [
            (256u16, 3u16, 1u32, 1u32),
            (257, 3, 1, 1),
            (258, 3, 1, 8),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 4, 8_000_000, 4096),
            (277, 3, 1, 1),
            (278, 3, 1, 1),
            (279, 4, 8_000_000, 4096),
        ] {
            strips.extend(tag.to_le_bytes());
            strips.extend(kind.to_le_bytes());
            strips.extend(count.to_le_bytes());
            strips.extend(value.to_le_bytes());
        }
        strips.extend([0; 4]);
        assert_eq!(strips.len(), 122);
        assert!(tiff_tags(&strips).is_err());
        assert!(render(ImageFormat::Tiff, &strips, 64, 64).is_err());

        // A render over the budget is refused before anything is decoded.
        let refused = plan(
            ImageFormat::Jpeg,
            ColorType::Rgb8,
            (12_000, 12_000),
            (64, 64),
            &progressive,
            header_room(ImageFormat::Jpeg, &progressive).unwrap(),
        );
        assert!(refused.is_err(), "{refused:?}");
    }
}
