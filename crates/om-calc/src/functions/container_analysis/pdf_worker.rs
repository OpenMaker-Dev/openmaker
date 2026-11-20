//! PDF data is read in a separate worker process, never in the caller's
//! (#566). `hayro` decodes a PDF's streams without any size limit: opening
//! the file already inflates its cross-reference and object streams, and
//! drawing or reading a page inflates content streams, fonts and images,
//! inline images included. A few hundred kilobytes can ask for gigabytes,
//! and a failed allocation aborts the whole process. So the client and
//! OpenMaker Server hand each read to a one-shot worker executable whose
//! global allocator refuses to go past [`MEMORY_BUDGET`] beyond its input
//! (the worker then aborts), and the caller:
//!
//! - kills the worker at a wall-clock deadline ([`Op::deadline`]), which
//!   bounds its CPU,
//! - reads at most [`Op::reply_limit`] bytes back,
//! - runs at most [`WORKERS`] of them at once,
//! - takes a worker that aborts, crashes, times out or answers badly for a
//!   PDF that does not read.
//!
//! The worker is an executable of its own: `om-gui-pdf-worker` beside
//! `om-gui`, `om-server-pdf-worker` beside `om-server` (each host
//! [`set_default`]s its sibling at startup), `om-pdf-worker` of this crate
//! for its tests and other hosts. `OPENMAKER_PDF_WORKER` names another.
//! Without one the PDF functions fail with error 3 rather than read the
//! data here. The worker's `main` is [`serve`], given the allocator's limit.
//!
//! Protocol, on the worker's stdin: `OMPDF1`, the operation (`u8`), the
//! picture's width and height (`u32` each, render; Find uses width as a
//! page index; Positions also uses height as a case-insensitive flag), the data's length
//! (`u64`) and the data; on its stdout: a status (`u8`, 0 read, 1 not a PDF
//! that reads), a length (`u64`) and the payload. Integers big-endian.
//! Assembling a new PDF from pages of others sends its sources and plan as
//! one data ([`assemble`]).

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{mpsc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::{pdf, pdf_assemble, pdf_highlight};

/// The environment variable naming a worker executable, over the host's.
pub const WORKER_VARIABLE: &str = "OPENMAKER_PDF_WORKER";
/// What the worker may allocate beyond its input.
pub const MEMORY_BUDGET: usize = 256 << 20;
/// The most PDF data sent to a worker.
const MAX_INPUT: u64 = 1 << 30;
/// Workers running at once in one process.
const WORKERS: usize = 2;
const MAGIC: &[u8; 6] = b"OMPDF1";

/// No worker executable is known or it cannot start: the PDF functions
/// fail with error 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unavailable;

impl From<Unavailable> for crate::error::CalcError {
    fn from(_: Unavailable) -> Self {
        crate::error::CalcError::from_code(3).expect("nonzero")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    PageSize = 1,
    Render = 2,
    Text = 3,
    Info = 4,
    Assemble = 5,
    Find = 6,
    Positions = 7,
    AnnotationFree = 8,
    PageSizes = 9,
    RenderPage = 10,
}

impl Op {
    fn of(byte: u8) -> Option<Op> {
        match byte {
            1 => Some(Op::PageSize),
            2 => Some(Op::Render),
            3 => Some(Op::Text),
            4 => Some(Op::Info),
            5 => Some(Op::Assemble),
            6 => Some(Op::Find),
            7 => Some(Op::Positions),
            8 => Some(Op::AnnotationFree),
            9 => Some(Op::PageSizes),
            10 => Some(Op::RenderPage),
            _ => None,
        }
    }

    fn deadline(self) -> Duration {
        Duration::from_secs(match self {
            Op::PageSize | Op::Info | Op::AnnotationFree | Op::PageSizes => 15,
            Op::Render | Op::RenderPage | Op::Text | Op::Find | Op::Positions => 30,
            Op::Assemble => 60,
        })
    }

    /// The longest reply read back. A new document holds the objects its
    /// pages need, written out of any compressed object streams, so it may
    /// be larger than its sources, by a bounded factor.
    fn reply_limit(self, width: u32, height: u32, input: u64) -> u64 {
        match self {
            Op::PageSize => 8,
            Op::Find => 4,
            Op::AnnotationFree => 1,
            Op::Positions => MAX_HIGHLIGHTS as u64 * 32,
            Op::Render | Op::RenderPage => pixel_bytes(width, height).unwrap_or(u64::MAX),
            Op::PageSizes => 4 + MAX_VIEW_PAGES as u64 * 8,
            Op::Text => 64 << 20,
            // A page's flags and its five boxes at most.
            Op::Info => 5 + pdf_assemble::INFO_PAGES as u64 * (1 + 5 * 32),
            Op::Assemble => input.saturating_mul(8).clamp(1 << 20, MAX_INPUT),
        }
    }
}

static DEFAULT: OnceLock<PathBuf> = OnceLock::new();

/// The worker this process uses when `OPENMAKER_PDF_WORKER` names none: a
/// host's sibling worker executable. The first call wins.
pub fn set_default(path: PathBuf) {
    let _ = DEFAULT.set(path);
}

/// The worker executable beside the running one, named `name` (with the
/// platform's executable suffix).
pub fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

/// Whether a worker executable is known: PDF data can be read.
pub fn available() -> bool {
    worker().is_some()
}

fn worker() -> Option<PathBuf> {
    std::env::var_os(WORKER_VARIABLE)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| DEFAULT.get().cloned())
        .filter(|path| path.is_file())
}

/// A slot among the [`WORKERS`] that may run at once.
struct Permit;

static RUNNING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

impl Permit {
    fn take() -> Permit {
        let (running, freed) = &RUNNING;
        let mut count = running.lock().unwrap_or_else(|e| e.into_inner());
        while *count >= WORKERS {
            count = freed.wait(count).unwrap_or_else(|e| e.into_inner());
        }
        *count += 1;
        Permit
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let (running, freed) = &RUNNING;
        *running.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        freed.notify_one();
    }
}

/// Ask a worker; `Ok(None)` for data that does not read (or a worker that
/// failed on it), `Err` when there is no worker.
fn ask(op: Op, width: u32, height: u32, data: &[u8]) -> Result<Option<Vec<u8>>, Unavailable> {
    let path = worker().ok_or(Unavailable)?;
    if data.len() as u64 > MAX_INPUT {
        return Ok(None);
    }
    let _permit = Permit::take();
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Unavailable)?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    };
    let mut header = Vec::with_capacity(27);
    header.extend(MAGIC);
    header.push(op as u8);
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend((data.len() as u64).to_be_bytes());
    let limit = op.reply_limit(width, height, data.len() as u64);
    let reply = std::thread::scope(|scope| {
        scope.spawn(move || {
            // A worker that stops reading (it failed) ends the write.
            let _ = stdin
                .write_all(&header)
                .and_then(|()| stdin.write_all(data));
        });
        let (sent, received) = mpsc::channel();
        scope.spawn(move || {
            let _ = sent.send(read_reply(&mut stdout, limit));
        });
        let deadline = Instant::now() + op.deadline();
        let reply = received.recv_timeout(op.deadline()).ok().flatten();
        // A worker that answered exits at once; one that did not, or is
        // still running at the deadline, is killed. Its pipes close then,
        // which ends both threads.
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if reply.is_some() && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                _ => {
                    let _ = child.kill();
                    break child.wait().ok();
                }
            }
        };
        reply.filter(|_| status.is_some_and(|s| s.success()))
    });
    Ok(reply.and_then(|(status, payload)| (status == 0).then_some(payload)))
}

/// The worker's status and payload, its length at most `limit`.
fn read_reply(from: &mut impl Read, limit: u64) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 9];
    from.read_exact(&mut head).ok()?;
    let len = u64::from_be_bytes(head[1..9].try_into().ok()?);
    if len > limit {
        return None;
    }
    let mut payload = Vec::with_capacity(len as usize);
    from.take(len).read_to_end(&mut payload).ok()?;
    (payload.len() as u64 == len).then_some((head[0], payload))
}

/// The first page's size in points; see [`pdf::page_size`].
pub fn page_size(data: &[u8]) -> Result<Option<(u32, u32)>, Unavailable> {
    Ok(ask(Op::PageSize, 0, 0, data)?.and_then(|reply| {
        let (w, h) = reply.split_at_checked(4)?;
        Some((
            u32::from_be_bytes(w.try_into().ok()?),
            u32::from_be_bytes(h.try_into().ok()?),
        ))
    }))
}

/// The first page drawn at `width` x `height` pixels; see [`pdf::render`].
pub fn render(
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Option<image::RgbImage>, Unavailable> {
    // No worker could hold a larger picture; nor is the reply read.
    if width == 0
        || height == 0
        || pixel_bytes(width, height).is_none_or(|bytes| bytes > MEMORY_BUDGET as u64)
    {
        return Ok(None);
    }
    Ok(ask(Op::Render, width, height, data)?
        .and_then(|pixels| image::RgbImage::from_raw(width, height, pixels)))
}

/// The bytes of a `width` x `height` RGB picture; `None` past `u64`.
fn pixel_bytes(width: u32, height: u32) -> Option<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(3)
}

/// The most pages [`page_sizes`] describes: a longer PDF is viewed as its
/// first so many pages.
pub const MAX_VIEW_PAGES: usize = 10_000;

/// Every page's size in points (of its first [`MAX_VIEW_PAGES`] pages), as
/// [`page_size`] gives the first's; `Ok(None)` for data that does not read.
pub fn page_sizes(data: &[u8]) -> Result<Option<Vec<(u32, u32)>>, Unavailable> {
    Ok(ask(Op::PageSizes, 0, 0, data)?.and_then(|reply| {
        let (count, rest) = reply.split_at_checked(4)?;
        let count = u32::from_be_bytes(count.try_into().ok()?) as usize;
        if count > MAX_VIEW_PAGES || rest.len() != count * 8 {
            return None;
        }
        Some(
            rest.chunks_exact(8)
                .map(|size| {
                    let (w, h) = size.split_at(4);
                    (
                        u32::from_be_bytes(w.try_into().expect("4 bytes")),
                        u32::from_be_bytes(h.try_into().expect("4 bytes")),
                    )
                })
                .collect(),
        )
    }))
}

/// Page `page` (from 0) drawn at `width` x `height` pixels; see
/// [`render`]. `Ok(None)` for a page the PDF does not have.
pub fn render_page(
    data: &[u8],
    page: u32,
    width: u32,
    height: u32,
) -> Result<Option<image::RgbImage>, Unavailable> {
    if width == 0
        || height == 0
        || pixel_bytes(width, height).is_none_or(|bytes| bytes > MEMORY_BUDGET as u64)
        || data.len() as u64 > MAX_INPUT - 4
    {
        return Ok(None);
    }
    let mut request = page.to_be_bytes().to_vec();
    request.extend(data);
    Ok(ask(Op::RenderPage, width, height, &request)?
        .and_then(|pixels| image::RgbImage::from_raw(width, height, pixels)))
}

/// Decode the page render request inside the worker.
fn render_page_request(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (page, pdf) = data.split_at_checked(4)?;
    let page = u32::from_be_bytes(page.try_into().ok()?);
    pdf::render_page(pdf, page, width, height).map(image::RgbImage::into_raw)
}

/// The text of every page; see [`pdf::text`].
pub fn text(data: &[u8]) -> Result<Option<String>, Unavailable> {
    Ok(ask(Op::Text, 0, 0, data)?.and_then(|bytes| String::from_utf8(bytes).ok()))
}

/// The largest search pattern, in UTF-8 bytes (OpenMaker's bound).
pub const MAX_PATTERN: usize = 4096;

/// Whether a pattern is a nonempty single line within the worker's bound.
pub fn pattern_ok(pattern: &str) -> bool {
    !pattern.is_empty() && pattern.len() <= MAX_PATTERN && !pattern.chars().any(|c| c.is_control())
}

/// Count literal matches on page `page` (from 0), using the worker's text
/// extraction rules. No OCR; `None` for a page that cannot be read or an
/// invalid pattern. MBS parity is inferred, see docs/re/mbs-plugin.md 5.
pub fn find(data: &[u8], page: u32, pattern: &str) -> Result<Option<u32>, Unavailable> {
    if !pattern_ok(pattern)
        || data.len() as u64 > MAX_INPUT.saturating_sub(pattern.len() as u64 + 4)
    {
        return Ok(None);
    }
    let mut request = (pattern.len() as u32).to_be_bytes().to_vec();
    request.extend(pattern.as_bytes());
    request.extend(data);
    Ok(ask(Op::Find, page, 0, &request)?
        .and_then(|bytes| bytes.try_into().ok().map(u32::from_be_bytes)))
}

/// Decode the find request inside the worker, before opening the PDF.
fn find_parts(data: &[u8]) -> Option<(&[u8], &str)> {
    let len = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    if len > MAX_PATTERN {
        return None;
    }
    let pattern = std::str::from_utf8(data.get(4..4 + len)?).ok()?;
    if !pattern_ok(pattern) {
        return None;
    }
    Some((data.get(4 + len..)?, pattern))
}

fn find_request(data: &[u8], page: u32) -> Option<Vec<u8>> {
    let (pdf, pattern) = find_parts(data)?;
    pdf::find(pdf, page, pattern).map(|count| count.to_be_bytes().to_vec())
}

/// Locate complete horizontal outline matches. None if glyph-run geometry
/// cannot account for every match from the existing extractor. Coordinates
/// are tight glyph ink bounds, inferred rather than measured against MBS.
pub fn highlight_positions(
    data: &[u8],
    page: u32,
    pattern: &str,
    insensitive: bool,
) -> Result<Option<Vec<[f64; 4]>>, Unavailable> {
    if !pattern_ok(pattern)
        || data.len() as u64 > MAX_INPUT.saturating_sub(pattern.len() as u64 + 4)
    {
        return Ok(None);
    }
    let mut request = (pattern.len() as u32).to_be_bytes().to_vec();
    request.extend(pattern.as_bytes());
    request.extend(data);
    Ok(
        ask(Op::Positions, page, u32::from(insensitive), &request)?.and_then(|bytes| {
            if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_HIGHLIGHTS {
                return None;
            }
            bytes
                .chunks_exact(32)
                .map(|row| {
                    let mut rect = [0.0; 4];
                    for (value, bytes) in rect.iter_mut().zip(row.chunks_exact(8)) {
                        *value = f64::from_be_bytes(bytes.try_into().ok()?);
                    }
                    rect_ok(rect).then_some(rect)
                })
                .collect()
        }),
    )
}

fn positions_request(data: &[u8], page: u32, insensitive: u32) -> Option<Vec<u8>> {
    if insensitive > 1 {
        return None;
    }
    let (pdf, pattern) = find_parts(data)?;
    Some(
        pdf_highlight::positions(pdf, page, pattern, insensitive == 1)?
            .into_iter()
            .flatten()
            .flat_map(f64::to_be_bytes)
            .collect(),
    )
}

/// What [`assemble`] needs to know of a PDF before it copies pages of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub pages: u32,
    /// Whether its pages look the same in a new document, which carries
    /// no optional content configuration: it has none (`pdf_assemble`).
    /// [`assemble`] refuses pages of a PDF that has one.
    pub optional_content_kept: bool,
    /// With [`info_whole`], whether a new document of all its pages keeps
    /// everything else it holds too: no document-level parts, document
    /// information, file identifiers or encryption that the page copy
    /// would leave behind (`pdf_assemble::document_kept`); `None` from
    /// [`info`], which does not ask.
    pub document_kept: Option<bool>,
    /// The geometry of its first `INFO_PAGES` pages.
    geometry: Vec<PageGeometry>,
}

impl Info {
    /// Page `page`'s geometry (from 0); `None` for a page past those the
    /// worker describes.
    pub fn geometry(&self, page: u32) -> Option<&PageGeometry> {
        self.geometry.get(page as usize)
    }

    /// Whether page `page` (from 0) is turned (`/Rotate`), so that
    /// [`assemble_planned`] draws nothing over it; a page past those the
    /// worker describes counts as turned.
    pub fn turned(&self, page: u32) -> bool {
        self.geometry(page).is_none_or(|g| g.turned)
    }
}

/// A PDF's page count and what copying its pages keeps of how they look;
/// `Ok(None)` for data that does not read.
pub fn info(data: &[u8]) -> Result<Option<Info>, Unavailable> {
    info_asking(data, false)
}

/// [`info`], and whether a new document of all its pages keeps everything
/// the document holds ([`Info::document_kept`]), as appending to a PDF
/// asks. The request's width 1 asks for it, which a worker that does not
/// know it ignores: its answer then says it is not kept.
pub fn info_whole(data: &[u8]) -> Result<Option<Info>, Unavailable> {
    info_asking(data, true)
}

/// The Info reply's flags byte: bit 0 optional content kept, and only when
/// `whole` was asked bit 1 the whole document kept, so a host that does not
/// ask reads the reply it always did.
fn info_asking(data: &[u8], whole: bool) -> Result<Option<Info>, Unavailable> {
    Ok(ask(Op::Info, u32::from(whole), 0, data)?.and_then(|reply| {
        let (pages, rest) = reply.split_at_checked(4)?;
        let (kept, geometry) = rest.split_first()?;
        let pages = u32::from_be_bytes(pages.try_into().ok()?);
        let described = (pages as usize).min(pdf_assemble::INFO_PAGES);
        let geometry = pdf_assemble::decode_geometry(geometry, described)?;
        (*kept <= if whole { 3 } else { 1 }).then_some(Info {
            pages,
            optional_content_kept: *kept & 1 != 0,
            document_kept: whole.then_some(*kept & 2 != 0),
            geometry,
        })
    }))
}

/// Whether the selected source pages have no annotations. None for a
/// malformed PDF/page index or request past the worker limits. Used to
/// refuse imported annotation flattening before changing runtime state.
pub fn annotation_free(data: &[u8], pages: &[u32]) -> Result<Option<bool>, Unavailable> {
    if pages.len() > pdf_assemble::MAX_PAGES
        || data.len() as u64 + pages.len() as u64 * 4 > MAX_INPUT
    {
        return Ok(None);
    }
    let mut request = Vec::with_capacity(pages.len() * 4 + data.len());
    for page in pages {
        request.extend(page.to_be_bytes());
    }
    request.extend(data);
    Ok(
        ask(Op::AnnotationFree, pages.len() as u32, 0, &request)?.and_then(|bytes| {
            match bytes.as_slice() {
                [0] => Some(false),
                [1] => Some(true),
                _ => None,
            }
        }),
    )
}

pub use super::pdf_assemble::{
    BoxKind, PageGeometry, Planned, INFO_PAGES, MARKER, MAX_PAGES, MAX_SOURCES,
};
pub use super::pdf_form::{
    check_fits, export_ok, group_refusal, name_ok, rect_ok, uri_ok, Field, FieldKind, Highlight,
    Link, MAX_ACTIONS, MAX_CAPTION, MAX_COORD, MAX_FIELDS, MAX_HIGHLIGHTS, MAX_LINKS, MAX_SCRIPT,
    MAX_URI,
};

/// A new PDF of the `plan`'s pages, each `(source, page)` of `sources`
/// (indexes from 0), with what they refer to (`pdf_assemble`); `Ok(None)`
/// when a source does not read, the plan names a page no source has, or
/// the request passes the worker's limits.
pub fn assemble(sources: &[&[u8]], plan: &[(u32, u32)]) -> Result<Option<Vec<u8>>, Unavailable> {
    let plan: Vec<Planned> = plan.iter().copied().map(Planned::from).collect();
    assemble_planned(sources, &plan, &[], &[], &[])
}

/// [`assemble`], keeping everything the copied pages refer to or failing:
/// `Ok(None)` rather than a document in which a reference to an object
/// left behind (the catalog, a page-tree node, a page not copied) became
/// `null`, as appending to a PDF asks. The request's width 1 asks for it;
/// only a worker that also answers [`info_whole`] knows it, and a host
/// asks this only of a PDF that worker said it keeps whole.
pub fn assemble_whole(
    sources: &[&[u8]],
    plan: &[(u32, u32)],
) -> Result<Option<Vec<u8>>, Unavailable> {
    let plan: Vec<Planned> = plan.iter().copied().map(Planned::from).collect();
    assemble_request(sources, &plan, &[], &[], &[], &[], true)
}

/// [`assemble`], each page with its boxes set and an overlay drawn over it
/// ([`Planned`]), and `fields` written on their pages, in an interactive
/// form ([`Field`]).
pub fn assemble_planned(
    sources: &[&[u8]],
    plan: &[Planned],
    fields: &[Field],
    actions: &[String],
    links: &[Link],
) -> Result<Option<Vec<u8>>, Unavailable> {
    assemble_highlighted(sources, plan, fields, actions, links, &[])
}

/// `assemble_planned` with bounded highlights on planned pages, as
/// annotations or baked appearances according to `Highlight::flattened`.
pub fn assemble_highlighted(
    sources: &[&[u8]],
    plan: &[Planned],
    fields: &[Field],
    actions: &[String],
    links: &[Link],
    highlights: &[Highlight],
) -> Result<Option<Vec<u8>>, Unavailable> {
    assemble_request(sources, plan, fields, actions, links, highlights, false)
}

fn assemble_request(
    sources: &[&[u8]],
    plan: &[Planned],
    fields: &[Field],
    actions: &[String],
    links: &[Link],
    highlights: &[Highlight],
    whole: bool,
) -> Result<Option<Vec<u8>>, Unavailable> {
    if sources.len() > pdf_assemble::MAX_SOURCES
        || plan.len() > pdf_assemble::MAX_PAGES
        || fields.len() > MAX_FIELDS
        || actions.len() > MAX_ACTIONS
        || links.len() > MAX_LINKS
        || highlights.len() > MAX_HIGHLIGHTS
    {
        return Ok(None);
    }
    // No request past the worker's input limit is built (`bundle`).
    let Some(request) =
        pdf_assemble::bundle(sources, plan, fields, actions, links, highlights, MAX_INPUT)
    else {
        return Ok(None);
    };
    ask(Op::Assemble, u32::from(whole), 0, &request)
}

/// A worker executable's `main`: read one request from stdin, answer it on
/// stdout. `limit` caps the process's allocator at what it holds plus the
/// given number of bytes; the worker must not decode before it holds.
pub fn serve(limit: impl FnOnce(usize) -> bool) -> ExitCode {
    let Some((op, width, height, data)) = request(&mut io::stdin().lock()) else {
        return ExitCode::FAILURE;
    };
    if !limit(MEMORY_BUDGET) {
        return ExitCode::FAILURE;
    }
    let payload = match op {
        Op::PageSize => pdf::page_size(&data).map(|(w, h)| {
            let mut bytes = w.to_be_bytes().to_vec();
            bytes.extend(h.to_be_bytes());
            bytes
        }),
        Op::Render => pdf::render(&data, width, height).map(image::RgbImage::into_raw),
        Op::RenderPage => render_page_request(&data, width, height),
        Op::PageSizes => pdf::page_sizes(&data, MAX_VIEW_PAGES).map(|sizes| {
            let mut bytes = (sizes.len() as u32).to_be_bytes().to_vec();
            for (w, h) in sizes {
                bytes.extend(w.to_be_bytes());
                bytes.extend(h.to_be_bytes());
            }
            bytes
        }),
        Op::Text => pdf::text(&data).map(String::into_bytes),
        Op::Info => pdf_assemble::info(data, width == 1).map(|(pages, kept, whole, geometry)| {
            let mut bytes = pages.to_be_bytes().to_vec();
            bytes.push(u8::from(kept) | u8::from(whole == Some(true)) << 1);
            pdf_assemble::encode_geometry(&geometry, &mut bytes);
            bytes
        }),
        Op::AnnotationFree if height == 0 => {
            pdf_assemble::annotation_free(data, width).map(|free| vec![u8::from(free)])
        }
        Op::AnnotationFree => None,
        Op::Assemble => pdf_assemble::assemble(data, width == 1),
        Op::Find => find_request(&data, width),
        Op::Positions => positions_request(&data, width, height),
    };
    let mut out = io::stdout().lock();
    let (status, payload) = match &payload {
        Some(bytes) => (0u8, bytes.as_slice()),
        None => (1u8, &[][..]),
    };
    let written = out
        .write_all(&[status])
        .and_then(|()| out.write_all(&(payload.len() as u64).to_be_bytes()))
        .and_then(|()| out.write_all(payload))
        .and_then(|()| out.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn request(from: &mut impl Read) -> Option<(Op, u32, u32, Vec<u8>)> {
    let mut head = [0u8; 23];
    from.read_exact(&mut head).ok()?;
    if &head[..6] != MAGIC {
        return None;
    }
    let op = Op::of(head[6])?;
    let width = u32::from_be_bytes(head[7..11].try_into().ok()?);
    let height = u32::from_be_bytes(head[11..15].try_into().ok()?);
    let len = u64::from_be_bytes(head[15..23].try_into().ok()?);
    if len > MAX_INPUT {
        return None;
    }
    let mut data = Vec::with_capacity(len as usize);
    from.take(len).read_to_end(&mut data).ok()?;
    (data.len() as u64 == len).then_some((op, width, height, data))
}
