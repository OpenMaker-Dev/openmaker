//! Report SVG pictures are drawn in a worker process, never in om-gui's.
//! usvg and resvg 0.45 allocate and recurse in ways no check made before
//! them bounds: reference cycles through clip paths, masks, filters and
//! patterns recurse until the stack overflows; pattern content is cloned
//! for every shape it fills; a large arc flattens into billions of
//! segments; filter results take the filter's whole region. Each of those
//! aborts a process, which `catch_unwind` cannot stop. So, as PDF data is
//! read (`om_calc` `pdf_worker`, #566), om-gui hands each SVG to
//! `om-gui-pdf-worker --svg`, whose allocator refuses to go
//! [`MEMORY_BUDGET`] past its input (the worker then aborts), and:
//!
//! - kills it at [`DEADLINE`], which bounds its CPU,
//! - reads back at most the picture's pixels and a short header,
//! - runs one at a time,
//! - takes a worker that aborts, crashes, times out or answers badly for an
//!   SVG that does not draw.
//!
//! Without a worker, report SVG pictures refuse (error 3) rather than draw
//! here. The app names its worker at startup ([`set_default`]).
//!
//! Protocol, on the worker's stdin: `OMSVG1`, the raster's width and height
//! (`u32` each; 0 and 0: the SVG's own size at 288 dpi, within the limits),
//! the data's length (`u64`) and the data; on its stdout: a status (`u8`, 0
//! drawn, 1 refused), a length (`u64`) and the payload: the width and
//! height (`u32` each) and the straight-alpha RGBA rows, or the refusal's
//! text. Integers big-endian.

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
const MAGIC: &[u8; 6] = b"OMSVG1";
/// The longest refusal text read back.
const MAX_REASON: u64 = 4096;

static DEFAULT: OnceLock<PathBuf> = OnceLock::new();
/// One worker at a time.
static RUNNING: Mutex<()> = Mutex::new(());

/// The worker executable report SVGs go to (`om-gui-pdf-worker` beside
/// the app). The first call wins.
pub fn set_default(path: PathBuf) {
    let _ = DEFAULT.set(path);
}

/// A drawn picture: its size in pixels and straight-alpha RGBA rows.
pub type Drawn = (u32, u32, Vec<u8>);

/// Draw `data` with the app's worker; see [`draw_with`].
#[cfg_attr(test, allow(dead_code, reason = "unit tests draw in-process"))]
pub(super) fn draw(data: &[u8], size: Option<[u32; 2]>) -> Result<Drawn, String> {
    let worker = DEFAULT
        .get()
        .filter(|path| path.is_file())
        .ok_or("no SVG worker (om-gui-pdf-worker) is installed beside OpenMaker")?;
    draw_with(worker, data, size)
}

/// Draw the SVG `data` at `size` pixels (its own size when `None`) in the
/// worker `worker`.
pub fn draw_with(worker: &Path, data: &[u8], size: Option<[u32; 2]>) -> Result<Drawn, String> {
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
    let mut header = Vec::with_capacity(22);
    header.extend(MAGIC);
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
    let Some((size, data)) = request(&mut io::stdin().lock()) else {
        return ExitCode::FAILURE;
    };
    if !limit(MEMORY_BUDGET) {
        return ExitCode::FAILURE;
    }
    let (status, payload) = match super::static_picture::svg_here(&data, size) {
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

fn request(from: &mut impl Read) -> Option<(Option<[u32; 2]>, Vec<u8>)> {
    let mut head = [0u8; 22];
    from.read_exact(&mut head).ok()?;
    if &head[..6] != MAGIC {
        return None;
    }
    let width = u32::from_be_bytes(head[6..10].try_into().ok()?);
    let height = u32::from_be_bytes(head[10..14].try_into().ok()?);
    let len = u64::from_be_bytes(head[14..22].try_into().ok()?);
    if len > super::static_picture::MAX_BYTES {
        return None;
    }
    let mut data = Vec::with_capacity(len as usize);
    from.take(len).read_to_end(&mut data).ok()?;
    let size = (width != 0 || height != 0).then_some([width, height]);
    (data.len() as u64 == len).then_some((size, data))
}
