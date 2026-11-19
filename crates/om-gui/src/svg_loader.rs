//! Browse's SVG pictures and button icons. A layout's image library is
//! hostile input (AGENTS.md rule 6): a small SVG can make usvg and resvg
//! allocate or recurse far past anything a check made before them bounds,
//! and a failed allocation or a stack overflow aborts the process (#303).
//! So Browse draws each SVG in the capped worker process report pictures
//! use (`report::svg_worker`), off the UI thread, and paints it once it is
//! ready (nothing until then). What the worker does not draw (the images
//! an SVG names, which it never reads) is left out of the rest; an SVG
//! that does not draw at all, or with no worker installed, shows egui's
//! error mark, as a library image that does not decode does.
//!
//! This replaces `egui_extras`' SVG loader, which draws in this process
//! and reads the files an SVG's images name from the local disk.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use egui::load::{BytesPoll, ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{ColorImage, ViewportId};

use crate::report::svg_worker;

/// How often a window with an SVG still being drawn repaints, so that the
/// SVG is asked for again while it is on screen.
const POLL: Duration = Duration::from_millis(250);
/// An SVG not asked for this long is off screen: its drawing is skipped.
const FRESH: Duration = Duration::from_secs(1);
/// A raster not asked for this long is dropped when its SVG has another
/// size (a window resized over a stretched picture leaves one per size).
/// Times, not egui's pass numbers: each window is a viewport with passes
/// of its own.
const UNUSED: Duration = Duration::from_secs(10);
/// The most the drawn rasters may hold. Past it, rasters are dropped, the
/// sizes an SVG no longer shows at first ([`State::trim`]): a window
/// dragged over a stretched picture asks for a new size every frame.
const BUDGET: usize = 128 << 20;
/// A raster asked for this recently is on screen, and is not dropped.
const SHOWN: Duration = Duration::from_millis(100);

/// One raster of one SVG: drawn, refused, or still being drawn (`None`).
struct Entry {
    requested: Instant,
    result: Option<Result<Arc<ColorImage>, String>>,
}

#[derive(Default)]
struct State {
    cache: HashMap<String, HashMap<SizeHint, Entry>>,
    /// The drawing thread's queue, started with the first SVG.
    queue: Option<mpsc::Sender<Job>>,
}

impl State {
    fn entry(&mut self, uri: &str, hint: SizeHint) -> Option<&mut Entry> {
        self.cache.get_mut(uri)?.get_mut(&hint)
    }

    /// Drop drawn rasters, as of `now`, until they hold at most `budget`
    /// bytes: first those of sizes an SVG was asked for at before its
    /// latest, then any not asked for within [`FRESH`], each oldest first.
    /// A raster asked for within [`SHOWN`] stays, so what is on screen,
    /// in any window, is not drawn again.
    fn trim(&mut self, budget: usize, now: Instant) {
        let size = |entry: &Entry| match &entry.result {
            Some(Ok(image)) => image.pixels.len() * size_of::<egui::Color32>(),
            _ => 0,
        };
        let mut held: usize = self.cache.values().flat_map(|b| b.values()).map(size).sum();
        if held <= budget {
            return;
        }
        let mut droppable = Vec::new();
        for (uri, bucket) in &self.cache {
            let latest = bucket.values().map(|entry| entry.requested).max();
            for (hint, entry) in bucket {
                let age = now.saturating_duration_since(entry.requested);
                let superseded = Some(entry.requested) != latest;
                if size(entry) > 0 && age > SHOWN && (superseded || age > FRESH) {
                    droppable.push((!superseded, entry.requested, uri.clone(), *hint));
                }
            }
        }
        droppable.sort_by_key(|(current, requested, ..)| (*current, *requested));
        for (_, _, uri, hint) in droppable {
            if held <= budget {
                break;
            }
            if let Some(bucket) = self.cache.get_mut(&uri) {
                held -= bucket.remove(&hint).as_ref().map_or(0, size);
                if bucket.is_empty() {
                    self.cache.remove(&uri);
                }
            }
        }
    }

    /// Whether raster `hint` of `uri` is still to be drawn: not drawn or
    /// forgotten, and asked for within [`FRESH`]. One left undrawn is
    /// forgotten, so that the next paint asks again.
    fn wanted(&mut self, uri: &str, hint: SizeHint) -> bool {
        let Some(entry) = self.entry(uri, hint) else {
            return false;
        };
        if entry.result.is_some() {
            return false;
        }
        if entry.requested.elapsed() <= FRESH {
            return true;
        }
        if let Some(bucket) = self.cache.get_mut(uri) {
            bucket.remove(&hint);
            if bucket.is_empty() {
                self.cache.remove(uri);
            }
        }
        false
    }
}

struct Job {
    ctx: egui::Context,
    viewport: ViewportId,
    uri: String,
    hint: SizeHint,
    bytes: egui::load::Bytes,
    size: [u32; 2],
}

/// Draws `.svg` URIs (library images, `document::library_image`) in the
/// SVG worker. Only exact sizes are drawn (`SizeHint::Size` without
/// keeping the aspect ratio), which is what `egui::Image::paint_at` asks
/// for; the raster is at most the worker's largest, stretched over the
/// rectangle. The rasters it keeps hold at most [`BUDGET`] bytes beside
/// those on screen.
pub struct SvgLoader {
    state: Arc<Mutex<State>>,
    budget: usize,
}

impl Default for SvgLoader {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            budget: BUDGET,
        }
    }
}

impl SvgLoader {
    pub const ID: &'static str = egui::generate_loader_id!(SvgLoader);

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

/// Install [`SvgLoader`] on `ctx` once.
pub fn install(ctx: &egui::Context) {
    if !ctx.is_loader_installed(SvgLoader::ID) {
        ctx.add_image_loader(Arc::new(SvgLoader::default()));
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// `bytes` drawn at `size` pixels: in the worker, or here in unit tests.
fn draw(bytes: &[u8], size: [u32; 2]) -> Result<Arc<ColorImage>, String> {
    let picture = crate::report::browse_svg(bytes, size)?;
    let size = [picture.width, picture.height].map(|n| n as usize);
    Ok(Arc::new(ColorImage::from_rgba_unmultiplied(
        size,
        &picture.rgba,
    )))
}

/// Draw the jobs on `queue`, one at a time, while they are wanted.
fn draw_queue(state: Weak<Mutex<State>>, queue: mpsc::Receiver<Job>) {
    for job in queue {
        let Some(state) = state.upgrade() else {
            return;
        };
        if !lock(&state).wanted(&job.uri, job.hint) {
            continue;
        }
        let result = draw(&job.bytes, job.size);
        let stored = match lock(&state).entry(&job.uri, job.hint) {
            Some(entry) => {
                entry.result = Some(result);
                true
            }
            None => false,
        };
        // Never with the state locked: egui may hold its own lock while it
        // asks this loader (egui_extras' DecodeCache keeps the same rule).
        if stored {
            job.ctx.request_repaint_of(job.viewport);
        }
    }
}

impl ImageLoader for SvgLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str, size_hint: SizeHint) -> ImageLoadResult {
        if !egui::load::has_extension(uri, "svg") {
            return Err(LoadError::NotSupported);
        }
        let SizeHint::Size {
            width,
            height,
            maintain_aspect_ratio: false,
        } = size_hint
        else {
            return Err(LoadError::Loading(
                "SVG is drawn at an exact width and height only".into(),
            ));
        };
        if width == 0 || height == 0 {
            return Err(LoadError::Loading("an empty SVG raster".into()));
        }
        let cached = self.state().entry(uri, size_hint).map(|entry| {
            entry.requested = Instant::now();
            entry.result.clone()
        });
        let poll = match cached {
            Some(Some(Ok(image))) => return Ok(ImagePoll::Ready { image }),
            Some(Some(Err(error))) => return Err(LoadError::Loading(error)),
            Some(None) => Ok(ImagePoll::Pending { size: None }),
            None => self.start(ctx, uri, size_hint, [width, height]),
        };
        if matches!(poll, Ok(ImagePoll::Pending { .. })) {
            ctx.request_repaint_after(POLL);
        }
        poll
    }

    fn forget(&self, uri: &str) {
        self.state().cache.remove(uri);
    }

    fn forget_all(&self) {
        self.state().cache.clear();
    }

    fn byte_size(&self) -> usize {
        self.state()
            .cache
            .values()
            .flat_map(|bucket| bucket.values())
            .map(|entry| match &entry.result {
                Some(Ok(image)) => image.pixels.len() * size_of::<egui::Color32>(),
                Some(Err(error)) => error.len(),
                None => 0,
            })
            .sum()
    }

    fn end_pass(&self, _pass_index: u64) {
        let mut state = self.state();
        state.cache.retain(|_, bucket| {
            if bucket.len() >= 2 {
                bucket.retain(|_, entry| entry.requested.elapsed() <= UNUSED);
            }
            !bucket.is_empty()
        });
        state.trim(self.budget, Instant::now());
    }

    fn has_pending(&self) -> bool {
        self.state()
            .cache
            .values()
            .flat_map(|bucket| bucket.values())
            .any(|entry| entry.result.is_none() && entry.requested.elapsed() <= FRESH)
    }
}

impl SvgLoader {
    /// Queue `uri` to be drawn at `size` pixels (fitted to the worker's
    /// and the GPU's largest raster).
    fn start(
        &self,
        ctx: &egui::Context,
        uri: &str,
        hint: SizeHint,
        size: [u32; 2],
    ) -> ImageLoadResult {
        let bytes = match ctx.try_load_bytes(uri)? {
            BytesPoll::Ready { bytes, .. } => bytes,
            BytesPoll::Pending { size } => return Ok(ImagePoll::Pending { size }),
        };
        let max_side = ctx.input(|input| input.max_texture_side);
        let size = svg_worker::fit(size, u32::try_from(max_side).unwrap_or(u32::MAX));
        if cfg!(test) {
            // Unit tests draw in this process (`report::browse_svg`), at
            // once, so that a frame paints what it shows.
            let result = draw(&bytes, size);
            self.state()
                .cache
                .entry(uri.to_owned())
                .or_default()
                .insert(
                    hint,
                    Entry {
                        requested: Instant::now(),
                        result: Some(result.clone()),
                    },
                );
            return match result {
                Ok(image) => Ok(ImagePoll::Ready { image }),
                Err(error) => Err(LoadError::Loading(error)),
            };
        }
        let job = Job {
            ctx: ctx.clone(),
            viewport: ctx.viewport_id(),
            uri: uri.to_owned(),
            hint,
            bytes,
            size,
        };
        let mut state = self.state();
        state.cache.entry(uri.to_owned()).or_default().insert(
            hint,
            Entry {
                requested: Instant::now(),
                result: None,
            },
        );
        let queue = state.queue.get_or_insert_with(|| {
            let (sender, receiver) = mpsc::channel();
            let weak = Arc::downgrade(&self.state);
            // A thread that does not start leaves its SVGs undrawn.
            let _ = std::thread::Builder::new()
                .name("om-gui svg".into())
                .spawn(move || draw_queue(weak, receiver));
            sender
        });
        if queue.send(job).is_err() {
            let error = "the SVG drawing thread has ended".to_owned();
            if let Some(entry) = state.entry(uri, hint) {
                entry.result = Some(Err(error.clone()));
            }
            return Err(LoadError::Loading(error));
        }
        Ok(ImagePoll::Pending { size: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window dragged over a stretched SVG asks for a new size each
    /// frame: past the budget, the sizes it no longer shows go first,
    /// oldest first, and the latest stays (#303 review).
    #[test]
    fn superseded_sizes_are_dropped_past_the_budget() {
        let ctx = egui::Context::default();
        let loader = Arc::new(SvgLoader {
            budget: 1 << 20,
            ..SvgLoader::default()
        });
        ctx.add_image_loader(loader.clone());
        let uri = "bytes://test/stretched.svg";
        ctx.include_bytes(
            uri,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#
                .to_vec(),
        );
        let hint = |width| SizeHint::Size {
            width,
            height: 256,
            maintain_aspect_ratio: false,
        };
        // 16 sizes of 256 KiB, drawn at once in unit tests.
        for width in 256..272 {
            assert!(matches!(
                ctx.try_load_image(uri, hint(width)),
                Ok(ImagePoll::Ready { .. })
            ));
        }
        assert!(loader.byte_size() > 4 << 20);
        // Asked for a millisecond apart, in order of width.
        let start = Instant::now();
        for (hint, entry) in loader.state().cache.get_mut(uri).unwrap() {
            let SizeHint::Size { width, .. } = hint else {
                unreachable!()
            };
            entry.requested = start + Duration::from_millis(u64::from(width - 256));
        }
        // Just after: everything was asked for within `SHOWN`, so stays.
        let now = start + Duration::from_millis(15);
        loader.state().trim(1 << 20, now);
        assert!(loader.byte_size() > 4 << 20);
        // Later, the superseded sizes go, oldest first, the latest stays.
        loader.state().trim(1 << 20, now + 2 * SHOWN);
        assert!(loader.byte_size() <= 1 << 20, "{}", loader.byte_size());
        let state = loader.state();
        let kept: Vec<u32> = state.cache[uri]
            .keys()
            .map(|hint| match hint {
                SizeHint::Size { width, .. } => *width,
                _ => 0,
            })
            .collect();
        assert!(kept.contains(&271), "{kept:?}");
        assert!(kept.iter().all(|width| *width >= 268), "{kept:?}");
    }
}
