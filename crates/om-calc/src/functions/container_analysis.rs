//! Container analysis functions: `GetContainerAttribute`, `GetHeight`,
//! `GetWidth`, `GetThumbnail`, `ReadQRCode`, `GetTextFromPDF`,
//! `VerifyContainer` (spec `docs/re/calc-functions.md` 5.4 and the oracle
//! answers "Oracle answers (container analysis)" in 9). They read container
//! data's streams; the evaluator has the host load the data of a container
//! field it holds only by reference first ([`reads_container_data`],
//! [`crate::eval::CalcHost::container_data`]). `GetLiveText` and
//! `GetLiveTextAsJSON` need the operating system's text recognition and
//! are the host's (FileMaker Server: error 3).
//!
//! Data a calculation decoded is typed by its file name as FileMaker types
//! a stored file ([`media`]). Pictures are read from their headers
//! ([`picture`], [`exif`]), audio from its tags and frames ([`audio`]), PDF
//! with `hayro` ([`pdf`]) in a worker process ([`pdf_worker`]); thumbnails
//! are drawn with the `image` crate and
//! QR codes read with `rqrr`.
//!
//! Errors (oracle): `ReadQRCode` gives 413 for a value that is not
//! container data, 408 for data that is not a picture and 8 for a picture
//! without a readable code. The others give `?` without an error where
//! they do not apply; `GetHeight` and `GetWidth` give 0.

use super::{Function, Keyword};
use crate::error::CalcError;
use crate::value::{Container, Ctx, StreamType, Value, ValueType};

mod attributes;
mod audio;
mod exif;
mod media;
mod pdf;
mod pdf_assemble;
mod pdf_form;
mod pdf_highlight;
pub mod pdf_worker;
mod picture;
mod qr;
mod thumbnail;

use media::{Kind, Media, Subject};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("GetContainerAttribute", get_container_attribute),
    Function::pure("GetHeight", get_height),
    Function::pure("GetTextFromPDF", get_text_from_pdf),
    Function::pure("GetThumbnail", get_thumbnail),
    Function::pure("GetWidth", get_width),
    Function::pure("ReadQRCode", read_qr_code),
    Function::pure("VerifyContainer", verify_container),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

/// Streams as a container field stores them, and the `FNAM` path type.
pub type StoredStreams = (Vec<(StreamType, Vec<u8>)>, &'static str);

/// The streams FileMaker stores for container data a calculation made when
/// a container field takes it (script oracle s34: `Set Field`), and the
/// path type of its `FNAM` (containers.md 2.4). Decoded data (one `FILE`
/// stream) is typed by its file name's extension as [`Kind::of_file`]
/// types it: a picture under its picture type with `SIZE` when its header
/// reads and `DPI_` when it has a resolution, a PDF, sound or movie under
/// its type (`movie` paths), anything else as `FILE`. Data with typed
/// streams (a field's data, a thumbnail) is stored as it is. FileMaker
/// also stores a converted preview of BMP, TIFF, WebP, HEIC and PDF data
/// (a `JPEG` or `PNGf` stream), which OpenMaker does not make. `None` for
/// a container without data.
pub fn stored_streams(c: &Container) -> Option<StoredStreams> {
    let (kind, data) = c.main_stream()?;
    if let Some(path) = c.path_type {
        return Some((c.streams.clone(), path));
    }
    if *kind != StreamType::FILE {
        let picture = picture::read(data).is_some() || matches!(&kind.0, b"SVG " | b"HEIC");
        let path = match Kind::of_stream(*kind) {
            Kind::File if picture => "image",
            k => k.path_type(),
        };
        return Some((c.streams.clone(), path));
    }
    let name = c.text.as_str();
    let kind = Kind::of_file(name, data);
    let mut streams = vec![(kind.stream_type(&Kind::extension(name)), data.clone())];
    if kind.is_picture() {
        if let Some(info) = picture::read(data) {
            let pair = |a: u32, b: u32| {
                let mut v = (a.min(0xffff) as u16).to_be_bytes().to_vec();
                v.extend((b.min(0xffff) as u16).to_be_bytes());
                v
            };
            streams.push((StreamType(*b"SIZE"), pair(info.width, info.height)));
            if info.resolution.is_some() {
                let (x, y) = info.dpi();
                streams.push((StreamType(*b"DPI_"), pair(x, y)));
            }
        }
    }
    // A PDF FileMaker could not render keeps a `file` path (s34).
    let path = if kind == Kind::Pdf {
        "file"
    } else {
        kind.path_type()
    };
    Some((streams, path))
}

/// Data named `name` as a step stores it that types it by another name,
/// `type_name` (Insert from URL types by the URL's last path segment and
/// names by the response's Content-Disposition; the Insert steps by the
/// file's own name; script oracle s34-containers-http): the streams of
/// [`stored_streams`] for `type_name`, the text `name`, and the `FNAM`
/// path type `path_type` (else the one the typing gives).
pub fn typed_file(
    name: &str,
    type_name: &str,
    data: Vec<u8>,
    path_type: Option<&'static str>,
) -> Container {
    let (streams, path) =
        stored_streams(&Container::file(type_name, data)).expect("a file has data");
    Container {
        text: crate::text::Text::plain(name),
        streams,
        path_type: Some(path_type.unwrap_or(path)),
        ..Container::default()
    }
}

/// Whether the function named `name` reads its first argument's container
/// data, so that the evaluator must load a referenced container's streams
/// before calling it.
pub(crate) fn reads_container_data(name: &str) -> bool {
    FUNCTIONS.iter().any(|f| f.name.eq_ignore_ascii_case(name))
}

/// Whether a call of the function named `name` with `args` answers from
/// a container's reference alone, without loading its streams:
/// `GetContainerAttribute ( field ; "filename" )` of a container a host
/// read from a field, whose text is the file name its header holds
/// (`FNAM`, containers.md 2.4). An unnamed one loads them, since data with
/// a main stream but no name is data, not empty.
pub(crate) fn answers_from_reference(name: &str, args: &[Value], cx: &Ctx) -> bool {
    if !name.eq_ignore_ascii_case("GetContainerAttribute") {
        return false;
    }
    let (Some(Value::Container(c)), Some(attribute)) = (args.first(), args.get(1)) else {
        return false;
    };
    c.is_unloaded()
        && !c.text.is_empty()
        && attributes::key(&attribute.to_text(cx).into_string()) == "filename"
}

type Result<T> = std::result::Result<T, CalcError>;

const fn err(code: u16) -> CalcError {
    match CalcError::from_code(code) {
        Some(e) => e,
        None => unreachable!(),
    }
}

/// 8: no QR code in the picture ("Empty result").
const NO_CODE: CalcError = err(8);
/// 408: container data that is not a picture.
const NOT_A_PICTURE: CalcError = err(408);
/// 413: a value that is not container data.
const NOT_CONTAINER_DATA: CalcError = err(413);

fn subject<'a>(v: &'a Value, cx: &Ctx) -> Subject<'a> {
    Subject::of(v, || v.to_text(cx).into_string())
}

fn get_container_attribute(args: &[Value], cx: &Ctx) -> Result<Value> {
    let name = args[1].to_text(cx).into_string();
    attributes::get(&subject(&args[0], cx), &name, cx)
}

/// The displayed size of a picture (after EXIF orientation), the first
/// page of a PDF in points; 0 for anything else.
fn size(v: &Value, cx: &Ctx) -> Result<(u32, u32)> {
    let Subject::Data(m) = subject(v, cx) else {
        return Ok((0, 0));
    };
    Ok(match m.kind {
        Kind::Pdf => pdf_worker::page_size(m.data)?,
        k if k.is_picture() => m
            .picture()
            .map(|p| p.display_size())
            .or_else(|| m.stored_size()),
        _ => None,
    }
    .unwrap_or((0, 0)))
}

fn get_width(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(Value::number(i64::from(size(&args[0], cx)?.0)))
}

fn get_height(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(Value::number(i64::from(size(&args[0], cx)?.1)))
}

/// `GetThumbnail ( field ; width ; height )`: see [`thumbnail`]. Empty
/// gives empty text; text and data that is neither a picture nor a PDF
/// `?`; a zero or empty box an empty result; a picture that does not
/// decode comes back as it is.
fn get_thumbnail(args: &[Value], cx: &Ctx) -> Result<Value> {
    let m = match subject(&args[0], cx) {
        Subject::Empty => return Ok(Value::empty_text()),
        Subject::Data(m) if m.kind.is_picture() || m.kind == Kind::Pdf => m,
        _ => return Ok(Value::invalid()),
    };
    let side = |v: &Value| v.to_number(cx).to_f64().map(f64::abs);
    let (Some(bw), Some(bh)) = (side(&args[1]), side(&args[2])) else {
        return Ok(Value::empty(ValueType::Container));
    };
    if bw == 0.0 || bh == 0.0 {
        return Ok(Value::empty(ValueType::Container));
    }
    let original = || Ok(Value::Container(m.container.clone()));
    if m.kind == Kind::Pdf {
        let Some((w, h)) = pdf_worker::page_size(m.data)? else {
            return Ok(Value::invalid());
        };
        let Some((tw, th)) = thumbnail::fit(w, h, bw, bh) else {
            return original();
        };
        let Some(page) = pdf_worker::render(m.data, tw, th)? else {
            return Ok(Value::invalid());
        };
        return Ok(made(
            &m,
            thumbnail::encode(&image::DynamicImage::ImageRgb8(page), false),
            tw,
            th,
            false,
        ));
    }
    let Some(info) = m.picture() else {
        return original();
    };
    let (w, h) = info.display_size();
    let turned = matches!(info.orientation(), Some(2..=8));
    let (tw, th) = match thumbnail::fit(w, h, bw, bh) {
        Some(size) => size,
        None if turned => (w, h),
        None => return original(),
    };
    let jpeg = m.kind == Kind::Jpeg;
    Ok(made(
        &m,
        thumbnail::scale(m.data, &info, tw, th, jpeg),
        tw,
        th,
        jpeg,
    ))
}

fn made(m: &Media, data: Option<Vec<u8>>, w: u32, h: u32, jpeg: bool) -> Value {
    match data {
        Some(data) => Value::Container(thumbnail::container(
            thumbnail::name(&m.name, jpeg),
            data,
            w,
            h,
            jpeg,
        )),
        None => Value::invalid(),
    }
}

fn read_qr_code(args: &[Value], cx: &Ctx) -> Result<Value> {
    match subject(&args[0], cx) {
        Subject::Empty => Ok(Value::invalid()),
        Subject::Text(_) => Err(NOT_CONTAINER_DATA),
        Subject::Data(m) if m.kind.is_picture() => qr::read(m.data).map(Value::text).ok_or(NO_CODE),
        _ => Err(NOT_A_PICTURE),
    }
}

fn get_text_from_pdf(args: &[Value], cx: &Ctx) -> Result<Value> {
    Ok(match subject(&args[0], cx) {
        Subject::Data(m) if m.kind == Kind::Pdf => {
            pdf_worker::text(m.data)?.map_or_else(Value::invalid, Value::text)
        }
        _ => Value::invalid(),
    })
}

/// `VerifyContainer`: `?` for any value a calculation holds, which has no
/// stored data to check (oracle); for a field's data, whether it is intact:
/// the host's answer when it read the data ([`Container::verified`]), or
/// the evaluator asks it for data held by reference
/// ([`crate::eval::CalcHost::container_data`]).
///
/// [`Container::verified`]: crate::Container::verified
fn verify_container(args: &[Value], _cx: &Ctx) -> Result<Value> {
    Ok(match &args[0] {
        Value::Container(c) => c.verified.map_or_else(Value::invalid, Value::boolean),
        _ => Value::invalid(),
    })
}
