//! SVG pictures are drawn in a worker process, never in om-gui's: report
//! pictures and icons, and Browse's pictures and icons (`svg_loader`,
//! #303). usvg and resvg 0.45 allocate and recurse in ways no check made
//! before them bounds: reference cycles through clip paths, masks, filters
//! and patterns recurse until the stack overflows; pattern content is
//! cloned for every shape it fills, and markers for every vertex they
//! mark; a large arc flattens into billions of segments; filter results
//! take the filter's whole region. Each of those aborts a process, which
//! `catch_unwind` cannot stop. So, as PDF data is read (`om_calc`
//! `pdf_worker`, #566), om-gui hands each SVG to `om-gui-pdf-worker
//! --svg`, whose allocator refuses to go [`MEMORY_BUDGET`] past its input
//! (the worker then aborts), and:
//!
//! - kills it at [`DEADLINE`], which bounds its CPU,
//! - reads back at most the picture's pixels and a short header,
//! - runs one at a time,
//! - takes a worker that aborts, crashes, times out or answers badly for an
//!   SVG that does not draw.
//!
//! The worker reads nothing but the SVG's own bytes: the images an SVG
//! names are never read, and its text uses the bundled fonts. Without a
//! worker, report SVG pictures refuse (error 3) rather than draw here, and
//! Browse shows its error mark for them. The app names its worker at
//! startup ([`set_default`]).
//!
//! Protocol, on the worker's stdin: `OMSVG2`, the [`Unsupported`] mode
//! (`u8`, 0 refuse, 1 skip), the raster's width and height (`u32` each; 0
//! and 0: the SVG's own size at 288 dpi, within the limits), the data's
//! length (`u64`) and the data; on its stdout: a status (`u8`, 0 drawn, 1
//! refused), a length (`u64`) and the payload: the width and height (`u32`
//! each) and the straight-alpha RGBA rows, or the refusal's text. Integers
//! big-endian.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The worker executable's argument that asks for an SVG.
pub const ARG: &str = "--svg";
/// What the worker may allocate beyond its input: the largest canvas
/// (64 MiB) and resvg's layers, filters and tiles.
pub const MEMORY_BUDGET: usize = 512 << 20;
/// How long one picture may take.
pub const DEADLINE: Duration = Duration::from_secs(30);
const MAGIC: &[u8; 6] = b"OMSVG2";
/// The request header: magic, mode, width, height and length.
const HEADER: usize = 23;
/// The longest refusal text read back.
const MAX_REASON: u64 = 4096;

static DEFAULT: OnceLock<PathBuf> = OnceLock::new();
/// One worker at a time.
static RUNNING: Mutex<()> = Mutex::new(());

/// What an SVG holds that OpenMaker does not draw: images, entity
/// declarations, references outside the picture, and more nodes or deeper
/// nesting than a report picture may have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Refuse the picture: a report fails (error 3) rather than print
    /// something other than what the layout shows.
    Refuse,
    /// Draw the rest, as Browse does (images left out), within the
    /// worker's memory and time limits. Report icons are drawn this way
    /// too, as Browse draws them.
    Skip,
}

/// The worker executable SVGs go to (`om-gui-pdf-worker` beside the app).
/// The first call wins.
pub fn set_default(path: PathBuf) {
    let _ = DEFAULT.set(path);
}

/// A drawn picture: its size in pixels and straight-alpha RGBA rows.
pub type Drawn = (u32, u32, Vec<u8>);

/// Draw `data` with the app's worker; see [`draw_with`].
#[cfg_attr(test, allow(dead_code, reason = "unit tests draw in-process"))]
pub(super) fn draw(
    data: &[u8],
    size: Option<[u32; 2]>,
    unsupported: Unsupported,
) -> Result<Drawn, String> {
    let worker = DEFAULT
        .get()
        .filter(|path| path.is_file())
        .ok_or("no SVG worker (om-gui-pdf-worker) is installed beside OpenMaker")?;
    draw_with(worker, data, size, unsupported)
}

/// The largest raster at most `size` pixels, in proportion, that the
/// worker draws: [`super::MAX_PICTURE_SIDE`] a side and
/// `MAX_SVG_PIXELS` in all, and at most `max_side` a side (the largest
/// texture). Each side is at least one pixel.
pub(crate) fn fit(size: [u32; 2], max_side: u32) -> [u32; 2] {
    let [width, height] = size.map(|n| f64::from(n.max(1)));
    let side = f64::from(max_side.clamp(1, super::MAX_PICTURE_SIDE));
    let scale = 1.0_f64
        .min(side / width.max(height))
        .min((super::static_picture::MAX_SVG_PIXELS / width / height).sqrt());
    [width, height].map(|n| ((n * scale).floor() as u32).max(1))
}

/// Draw the SVG `data` at `size` pixels (its own size when `None`) in the
/// worker `worker`.
pub fn draw_with(
    worker: &Path,
    data: &[u8],
    size: Option<[u32; 2]>,
    unsupported: Unsupported,
) -> Result<Drawn, String> {
    let [width, height] = size.unwrap_or([0, 0]);
    let limit = match size {
        Some([w, h]) => u64::from(w) * u64::from(h) * 4,
        None => super::static_picture::MAX_SVG_PIXELS as u64 * 4,
    };
    let _one = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let mut child = Command::new(worker)
        .arg(ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the SVG worker did not start: {e}"))?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("the SVG worker did not start".into());
    };
    let mut header = Vec::with_capacity(HEADER);
    header.extend(MAGIC);
    header.push(match unsupported {
        Unsupported::Refuse => 0,
        Unsupported::Skip => 1,
    });
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend((data.len() as u64).to_be_bytes());
    let reply = std::thread::scope(|scope| {
        scope.spawn(move || {
            // A worker that stops reading (it failed) ends the write.
            let _ = stdin
                .write_all(&header)
                .and_then(|()| stdin.write_all(data));
        });
        let (sent, received) = mpsc::channel();
        scope.spawn(move || {
            let _ = sent.send(read_reply(&mut stdout, limit + 8));
        });
        let deadline = Instant::now() + DEADLINE;
        let reply = received.recv_timeout(DEADLINE).ok().flatten();
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
    let Some((status, payload)) = reply else {
        return Err("the SVG could not be drawn within the worker's memory and time limits".into());
    };
    if status != 0 {
        let reason = String::from_utf8_lossy(&payload[..payload.len().min(MAX_REASON as usize)]);
        return Err(reason.into_owned());
    }
    let (head, rgba) = payload
        .split_at_checked(8)
        .ok_or("the SVG worker answered badly")?;
    let w = u32::from_be_bytes(head[..4].try_into().map_err(|_| "bad answer")?);
    let h = u32::from_be_bytes(head[4..].try_into().map_err(|_| "bad answer")?);
    let fits = size.is_none_or(|s| s == [w, h])
        && w > 0
        && h > 0
        && u64::from(w) * u64::from(h) * 4 == rgba.len() as u64;
    if !fits {
        return Err("the SVG worker answered badly".into());
    }
    Ok((w, h, rgba.to_vec()))
}

/// The worker's status and payload, its length at most `limit` (a refusal's
/// text at most [`MAX_REASON`]).
fn read_reply(from: &mut impl Read, limit: u64) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 9];
    from.read_exact(&mut head).ok()?;
    let len = u64::from_be_bytes(head[1..9].try_into().ok()?);
    if len > if head[0] == 0 { limit } else { MAX_REASON } {
        return None;
    }
    let mut payload = Vec::with_capacity(len as usize);
    from.take(len).read_to_end(&mut payload).ok()?;
    (payload.len() as u64 == len).then_some((head[0], payload))
}

/// The worker's `main` for `--svg`: read one request, cap the allocator
/// with `limit` ([`MEMORY_BUDGET`] past what the request took), draw, and
/// answer.
pub fn serve(limit: impl FnOnce(usize) -> bool) -> ExitCode {
    let Some((unsupported, size, data)) = request(&mut io::stdin().lock()) else {
        return ExitCode::FAILURE;
    };
    if !limit(MEMORY_BUDGET) {
        return ExitCode::FAILURE;
    }
    let (status, payload) = match super::static_picture::svg_here(&data, size, unsupported) {
        Ok(picture) => {
            let mut bytes = Vec::with_capacity(8 + picture.rgba.len());
            bytes.extend(picture.width.to_be_bytes());
            bytes.extend(picture.height.to_be_bytes());
            bytes.extend(&picture.rgba);
            (0u8, bytes)
        }
        Err(reason) => {
            let mut bytes = reason.into_bytes();
            bytes.truncate(MAX_REASON as usize);
            (1u8, bytes)
        }
    };
    let mut out = io::stdout().lock();
    let written = out
        .write_all(&[status])
        .and_then(|()| out.write_all(&(payload.len() as u64).to_be_bytes()))
        .and_then(|()| out.write_all(&payload))
        .and_then(|()| out.flush());
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

type Request = (Unsupported, Option<[u32; 2]>, Vec<u8>);

fn request(from: &mut impl Read) -> Option<Request> {
    let mut head = [0u8; HEADER];
    from.read_exact(&mut head).ok()?;
    if &head[..6] != MAGIC {
        return None;
    }
    let unsupported = match head[6] {
        0 => Unsupported::Refuse,
        1 => Unsupported::Skip,
        _ => return None,
    };
    let width = u32::from_be_bytes(head[7..11].try_into().ok()?);
    let height = u32::from_be_bytes(head[11..15].try_into().ok()?);
    let len = u64::from_be_bytes(head[15..23].try_into().ok()?);
    if len > super::static_picture::MAX_BYTES {
        return None;
    }
    let mut data = Vec::with_capacity(len as usize);
    from.take(len).read_to_end(&mut data).ok()?;
    let size = (width != 0 || height != 0).then_some([width, height]);
    (data.len() as u64 == len).then_some((unsupported, size, data))
}
