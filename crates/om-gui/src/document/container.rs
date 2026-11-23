//! Container fields in Browse (containers.md 6): a picture scaled into the
//! field by its object's graphic format (data-format.md "Graphic format"),
//! any other file as an icon above its name.
//!
//! A value a stored field names by its library member shows from the
//! member's header first: an `FNAM` path that is not an `image` path is a
//! file or media, shown without downloading its data, except a file named
//! `.pdf`: a PDF shows its first page (FileMaker's own preview of one is
//! an `image` path, containers.md 6.2). A picture's data
//! comes through the window's calculation cache (`CalcNeed::MemberData`)
//! and is decoded on background threads, never while painting; until it is
//! ready the field shows empty, or the same picture at its last size.
//!
//! Data is hostile: decoding is bounded in size and memory, a decoder's
//! panic is caught, and data that does not decode shows as an empty field.
//! Pictures are kept as textures of the size they show at, by the data's
//! MD5, the frame's size in pixels and the format, within a byte budget, so
//! scrolling a list or painting again never decodes again.

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};

use egui::{pos2, vec2, Color32, Rect, TextureHandle, TextureOptions, Ui};
use om_data::calc::Container;
use om_layout::{GraphicFormat, ResolvedStyle};

use crate::calc::Evaluator;
use crate::icons::{self, Icon};

/// The largest picture decoded, per side, and the most memory one decode
/// may allocate.
const MAX_SIDE: u32 = 16_384;
const MAX_DECODE_BYTES: u64 = 256 << 20;
/// The largest texture side made: a frame larger than this in pixels
/// shows the picture stretched from this size.
const MAX_TEXTURE_SIDE: u32 = 4096;
/// Texture bytes kept; past it the least recently painted go. Pictures
/// painted in the last passes stay, so the pictures on screen, which the
/// screen bounds, never decode again for want of room.
const TEXTURE_BUDGET: usize = 128 << 20;
/// Pictures decoding at once, which also bounds the data copies held for
/// them and the member downloads asked for in a frame.
const IN_FLIGHT: usize = 6;
const WORKERS: usize = 2;
/// Decisions kept by data; past it they are made again.
const KINDS: usize = 8192;
/// A file's icon, in points (OpenMaker's choice).
const FILE_ICON: f32 = 32.0;

/// What container data that shows no picture shows, by its MD5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// No picture stream: a file, media, or a picture type not decoded.
    File,
    /// A picture stream that does not decode.
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    data: [u8; 16],
    /// The frame's size in physical pixels: a page's own, for a page of
    /// the PDF view.
    frame: [u32; 2],
    format: u16,
    /// Physical pixels per layout point, as bits.
    scale: u32,
    /// A page of the PDF view (#200), from 0; `None` for what a field
    /// shows by its graphic format.
    page: Option<u32>,
}

struct Texture {
    handle: TextureHandle,
    /// The picture's size, after its EXIF orientation: pixels, or a PDF
    /// page's points.
    natural: [u32; 2],
    /// The part of the picture it holds, as fractions of its width and
    /// height: [left, top, right, bottom].
    source: [f32; 4],
    bytes: usize,
    last: u64,
}

struct Job {
    key: Key,
    data: Arc<[u8]>,
    making: Making,
    frame: [f32; 2],
    scale: f32,
    format: GraphicFormat,
}

/// A PDF's page sizes in points, as the PDF view lays its pages out.
type PageSizes = Arc<[[u32; 2]]>;

/// What a job makes of its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Making {
    /// A picture placed by the field's format.
    Picture,
    /// A PDF's first page placed by the field's format.
    FirstPage,
    /// A page of a PDF at the key's size in pixels, whole (#200).
    Page(u32),
    /// Every page's size of a PDF (#200).
    Sizes,
}

struct Decoded {
    natural: [u32; 2],
    source: [f32; 4],
    image: egui::ColorImage,
}

enum Done {
    Picture {
        key: Key,
        decoded: Option<Decoded>,
    },
    /// A PDF's page sizes in points; `None` for data that does not read.
    Sizes {
        data: [u8; 16],
        sizes: Option<Vec<[u32; 2]>>,
    },
}

struct Workers {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
}

/// The window's decoded container pictures.
#[derive(Default)]
pub(crate) struct Pictures {
    textures: HashMap<Key, Texture>,
    kinds: HashMap<[u8; 16], Kind>,
    decoding: HashSet<Key>,
    bytes: usize,
    workers: Option<Workers>,
    /// Pass number and how many reads this pass started.
    started: (u64, usize),
    /// The page sizes of PDF data the PDF view shows, by its MD5 (`None`
    /// for data that does not read), and those being read.
    page_sizes: HashMap<[u8; 16], Option<PageSizes>>,
    sizing: HashSet<[u8; 16]>,
    /// Pages of the PDF view that did not draw, not asked for again.
    failed_pages: HashSet<Key>,
    /// Interactive containers' audio and video, as files the player opens.
    media: Media,
}

/// An interactive container's audio and video, copied to files the
/// window's player opens: a private folder made when first needed and
/// removed with the window (#200).
#[derive(Default)]
struct Media {
    /// Shared with the copies being written, so it is removed only once
    /// the last of them ends.
    folder: Option<Arc<tempfile::TempDir>>,
    files: HashMap<[u8; 16], MediaFile>,
    written: Option<MediaChannel>,
}

type MediaChannel = (
    mpsc::Sender<([u8; 16], Option<[String; 2]>)>,
    mpsc::Receiver<([u8; 16], Option<[String; 2]>)>,
);

/// Where a media file the player opens is.
enum MediaFile {
    Writing,
    /// The pages that play the copy: waiting for the user's play control,
    /// and starting by itself.
    Ready([String; 2]),
    Failed,
}

/// The largest media file the player is handed.
const MAX_MEDIA: usize = 1 << 30;
/// Media copies written at once in a window: each holds its data in memory
/// until written.
const MEDIA_WRITERS: usize = 2;
/// Why a media file that could not be copied for the player does not play.
const MEDIA_FAILED: &str = "This file could not be opened.";

impl Pictures {
    /// Results of finished decodes, as textures.
    fn collect(&mut self, ctx: &egui::Context) {
        if let Some((_, written)) = &self.media.written {
            for (data, address) in written.try_iter() {
                self.media
                    .files
                    .insert(data, address.map_or(MediaFile::Failed, MediaFile::Ready));
            }
        }
        let Some(workers) = &self.workers else {
            return;
        };
        let pass = ctx.cumulative_pass_nr();
        let finished: Vec<Done> = workers.done.try_iter().collect();
        for done in finished {
            let (key, decoded) = match done {
                Done::Picture { key, decoded } => (key, decoded),
                Done::Sizes { data, sizes } => {
                    self.sizing.remove(&data);
                    if self.page_sizes.len() >= KINDS {
                        self.page_sizes.clear();
                    }
                    self.page_sizes.insert(data, sizes.map(Arc::from));
                    continue;
                }
            };
            self.decoding.remove(&key);
            let Some(decoded) = decoded else {
                // A page that does not draw leaves the data's other pages,
                // and is not asked for again.
                if key.page.is_none() {
                    self.remember(key.data, Kind::Unreadable);
                } else {
                    if self.failed_pages.len() >= KINDS {
                        self.failed_pages.clear();
                    }
                    self.failed_pages.insert(key);
                }
                continue;
            };
            let bytes = decoded.image.pixels.len() * 4;
            let handle =
                ctx.load_texture("container picture", decoded.image, TextureOptions::LINEAR);
            self.bytes += bytes;
            if let Some(old) = self.textures.insert(
                key,
                Texture {
                    handle,
                    natural: decoded.natural,
                    source: decoded.source,
                    bytes,
                    last: pass,
                },
            ) {
                self.bytes -= old.bytes;
            }
        }
        // The least recently painted go first; never one still on screen
        // (painted in this pass or the two before it).
        while self.bytes > TEXTURE_BUDGET {
            let Some((&key, _)) = self
                .textures
                .iter()
                .filter(|(_, texture)| texture.last + 2 < pass)
                .min_by_key(|(_, texture)| texture.last)
            else {
                break;
            };
            if let Some(texture) = self.textures.remove(&key) {
                self.bytes -= texture.bytes;
            }
        }
    }

    fn remember(&mut self, data: [u8; 16], kind: Kind) {
        if self.kinds.len() >= KINDS && !self.kinds.contains_key(&data) {
            self.kinds.clear();
        }
        self.kinds.insert(data, kind);
    }

    /// Whether another read or decode may start this pass.
    fn may_start(&mut self, pass: u64) -> bool {
        if self.started.0 != pass {
            self.started = (pass, 0);
        }
        // A PDF's page sizes being read count too (#200).
        self.decoding.len() + self.sizing.len() + self.started.1 < IN_FLIGHT
    }

    fn submit(&mut self, ctx: &egui::Context, job: Job) {
        let workers = self.workers.get_or_insert_with(|| spawn(ctx));
        let (key, making) = (job.key, job.making);
        if workers.jobs.send(job).is_ok() {
            match making {
                Making::Sizes => self.sizing.insert(key.data),
                _ => self.decoding.insert(key),
            };
        } else {
            self.workers = None;
        }
    }
}

fn spawn(ctx: &egui::Context) -> Workers {
    let (jobs, queue) = mpsc::channel::<Job>();
    let (finished, done) = mpsc::channel();
    let queue = Arc::new(Mutex::new(queue));
    for index in 0..WORKERS {
        let (queue, finished, ctx) = (Arc::clone(&queue), finished.clone(), ctx.clone());
        // A thread that cannot start leaves the others; with none, sending
        // fails and the next picture tries again.
        let _ = std::thread::Builder::new()
            .name(format!("container-pictures-{index}"))
            .spawn(move || loop {
                let job = match queue.lock().map(|queue| queue.recv()) {
                    Ok(Ok(job)) => job,
                    _ => return,
                };
                let done = match job.making {
                    Making::Sizes => Done::Sizes {
                        data: job.key.data,
                        sizes: catch_unwind(AssertUnwindSafe(|| page_sizes(&job.data)))
                            .ok()
                            .flatten(),
                    },
                    making => Done::Picture {
                        key: job.key,
                        decoded: catch_unwind(AssertUnwindSafe(|| match making {
                            Making::FirstPage => {
                                first_page(&job.data, job.frame, job.scale, job.format)
                            }
                            Making::Page(index) => page(&job.data, index, job.key.frame),
                            _ => decode(&job.data, job.frame, job.scale, job.format),
                        }))
                        .ok()
                        .flatten(),
                    },
                };
                if finished.send(done).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
    }
    Workers { jobs, done }
}

/// Where a picture of `natural` pixels goes in a frame of `frame` points:
/// [left, top, width, height] relative to the frame, in points, its corner
/// on a whole pixel. A picture pixel is a point (OpenMaker's choice, as
/// `GetWidth` counts pixels).
fn placement(format: GraphicFormat, natural: [u32; 2], frame: [f32; 2], scale: f32) -> [f32; 4] {
    let [x, y, w, h] = format.place(
        (f64::from(natural[0]), f64::from(natural[1])),
        (f64::from(frame[0]), f64::from(frame[1])),
    );
    let snap = |v: f64| ((v * f64::from(scale)).round() / f64::from(scale)) as f32;
    [snap(x), snap(y), w as f32, h as f32]
}

/// The part of the picture a frame shows, in the picture's pixels:
/// [left, top, right, bottom], whole pixels covering it; `None` when none
/// shows (which a placement in a frame of some size never gives).
fn visible_source(placed: [f32; 4], natural: [u32; 2], frame: [f32; 2]) -> Option<[u32; 4]> {
    let [x, y, w, h] = placed;
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let (sx, sy) = (natural[0] as f32 / w, natural[1] as f32 / h);
    let left = ((0.0 - x).max(0.0) * sx).floor();
    let top = ((0.0 - y).max(0.0) * sy).floor();
    let right = ((frame[0] - x).min(w) * sx).ceil();
    let bottom = ((frame[1] - y).min(h) * sy).ceil();
    let clamp = |v: f32, max: u32| v.clamp(0.0, max as f32) as u32;
    let source = [
        clamp(left, natural[0]),
        clamp(top, natural[1]),
        clamp(right, natural[0]),
        clamp(bottom, natural[1]),
    ];
    (source[2] > source[0] && source[3] > source[1]).then_some(source)
}

/// Decode `data` within the limits, turn it as its EXIF orientation says,
/// and keep the part the frame shows at the size it shows (never larger
/// than the picture's own pixels). `None` when it does not decode.
fn decode(data: &[u8], frame: [f32; 2], scale: f32, format: GraphicFormat) -> Option<Decoded> {
    use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
    let mut reader = ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    if !matches!(
        reader.format()?,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::Tiff
            | ImageFormat::WebP
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut picture = DynamicImage::from_decoder(decoder).ok()?;
    picture.apply_orientation(orientation);
    let natural = [picture.width(), picture.height()];
    if natural.contains(&0) {
        return None;
    }
    let placed = placement(format, natural, frame, scale);
    let source = visible_source(placed, natural, frame)?;
    let [left, top, right, bottom] = source;
    let (width, height) = (right - left, bottom - top);
    let shown = picture.crop_imm(left, top, width, height);
    // The size it shows at in pixels, at most its own and the texture limit.
    let target = |pixels: u32, shown: f32| {
        ((shown * scale).ceil() as u32)
            .clamp(1, pixels)
            .min(MAX_TEXTURE_SIDE)
    };
    let to_points = |pixels: u32, natural: u32, size: f32| pixels as f32 * size / natural as f32;
    let (tw, th) = (
        target(width, to_points(width, natural[0], placed[2])),
        target(height, to_points(height, natural[1], placed[3])),
    );
    let shown = if (tw, th) == (width, height) {
        shown
    } else {
        shown.resize_exact(tw, th, image::imageops::FilterType::CatmullRom)
    };
    let rgba = shown.into_rgba8();
    Some(Decoded {
        natural,
        source: [
            left as f32 / natural[0] as f32,
            top as f32 / natural[1] as f32,
            right as f32 / natural[0] as f32,
            bottom as f32 / natural[1] as f32,
        ],
        image: egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], rgba.as_raw()),
    })
}

/// The first page of PDF data where the format places it, a point of the
/// page to a point of the layout: drawn at the size it shows (at most
/// [`MAX_TEXTURE_SIDE`] pixels a side), and the part the frame shows kept.
/// `None` when the data is not a PDF.
fn first_page(data: &[u8], frame: [f32; 2], scale: f32, format: GraphicFormat) -> Option<Decoded> {
    use om_data::calc::functions::pdf_worker;
    let (width, height) = pdf_worker::page_size(data).ok().flatten()?;
    let natural = [width.max(1), height.max(1)];
    let placed = placement(format, natural, frame, scale);
    // The whole page drawn at the size it shows, its visible part kept.
    let side = |points: f32| ((points * scale).ceil() as u32).clamp(1, MAX_TEXTURE_SIDE);
    let (pw, ph) = (side(placed[2]), side(placed[3]));
    let [left, top, right, bottom] = visible_source(placed, [pw, ph], frame)?;
    let page = pdf_worker::render(data, pw, ph).ok().flatten()?;
    if page.dimensions() != (pw, ph) {
        return None;
    }
    let shown = image::DynamicImage::ImageRgb8(page)
        .crop_imm(left, top, right - left, bottom - top)
        .into_rgba8();
    Some(Decoded {
        natural,
        source: [
            left as f32 / pw as f32,
            top as f32 / ph as f32,
            right as f32 / pw as f32,
            bottom as f32 / ph as f32,
        ],
        image: egui::ColorImage::from_rgba_unmultiplied(
            [shown.width() as usize, shown.height() as usize],
            shown.as_raw(),
        ),
    })
}

/// Every page's size of PDF data, in points (`pdf_worker::page_sizes`):
/// `None` when it does not read, or has no page.
fn page_sizes(data: &[u8]) -> Option<Vec<[u32; 2]>> {
    use om_data::calc::functions::pdf_worker;
    let sizes = pdf_worker::page_sizes(data).ok().flatten()?;
    (!sizes.is_empty()).then(|| {
        sizes
            .into_iter()
            .map(|(w, h)| [w.max(1), h.max(1)])
            .collect()
    })
}

/// Page `index` of PDF data drawn whole at `pixels`, for the PDF view.
fn page(data: &[u8], index: u32, pixels: [u32; 2]) -> Option<Decoded> {
    use om_data::calc::functions::pdf_worker;
    let page = pdf_worker::render_page(data, index, pixels[0], pixels[1])
        .ok()
        .flatten()?;
    if page.dimensions() != (pixels[0], pixels[1]) {
        return None;
    }
    let rgba = image::DynamicImage::ImageRgb8(page).into_rgba8();
    Some(Decoded {
        natural: pixels,
        source: [0.0, 0.0, 1.0, 1.0],
        image: egui::ColorImage::from_rgba_unmultiplied(
            [pixels[0] as usize, pixels[1] as usize],
            rgba.as_raw(),
        ),
    })
}

/// Whether container field object `object` is set to "Optimize for:
/// Interactive content", which shows a PDF whose pages the user scrolls
/// and pages through ([`paint_pdf_view`], #200), rather than its first
/// page as a picture: bit `0x0100` of its graphic format (containers.md
/// 6.6, from FileMaker Pro 26.0.3's saves of one container both ways).
/// Its "Start playback automatically" is `GraphicFormat::autoplay`.
pub(super) fn is_interactive(object: &om_layout::LayoutObject) -> bool {
    object
        .graphic_format
        .is_some_and(om_layout::GraphicFormat::interactive)
}

/// Space between pages and around them in the PDF view, in points
/// (OpenMaker's choice [I: FileMaker Pro's interactive container is
/// unmeasured]). The pages sit on the under-page gray Preview uses.
const PAGE_GAP: f32 = 6.0;
/// The height of the page bar at the view's foot, in points.
const PAGE_BAR: f32 = 22.0;

/// PDF data's pages in `frame`, as an interactive container shows them
/// (#200): one under the other at the frame's width, scrolled through,
/// with a bar at its foot to go to the previous or next page that says
/// which page shows. Each page is drawn by the PDF worker when it comes
/// into view, at the size it shows, and kept as the field's pictures are.
/// `data` is the PDF's MD5; `bytes` gives its data when a page must be
/// drawn; `id` keeps the view's place between frames.
pub(super) fn paint_pdf_view(
    ui: &mut Ui,
    pictures: &mut Pictures,
    data: [u8; 16],
    bytes: &mut dyn FnMut() -> Option<Arc<[u8]>>,
    frame: Rect,
    id: egui::Id,
) {
    let ctx = ui.ctx().clone();
    pictures.collect(&ctx);
    if !frame.is_positive() || !ui.is_rect_visible(frame) {
        return;
    }
    let pass = ctx.cumulative_pass_nr();
    let sizes = match pictures.page_sizes.get(&data) {
        Some(Some(sizes)) => Arc::clone(sizes),
        // Not a PDF that reads: nothing to show.
        Some(None) => return,
        None => {
            if !pictures.sizing.contains(&data) && pictures.may_start(pass) {
                if let Some(bytes) = bytes() {
                    pictures.started.1 += 1;
                    let key = Key {
                        data,
                        frame: [0, 0],
                        format: 0,
                        scale: 0,
                        page: None,
                    };
                    pictures.submit(
                        &ctx,
                        Job {
                            key,
                            data: bytes,
                            making: Making::Sizes,
                            frame: [0.0, 0.0],
                            scale: 1.0,
                            format: GraphicFormat::default(),
                        },
                    );
                }
            }
            return;
        }
    };
    let scale = ui.pixels_per_point()
        * ctx
            .layer_transform_to_global(ui.layer_id())
            .map_or(1.0, |t| t.scaling);
    let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(frame));
    painter.rect_filled(frame, 0.0, crate::theme::PRINT_WORKSPACE);
    let pages = sizes.len();
    let bar = (pages > 1)
        .then(|| Rect::from_min_max(pos2(frame.left(), frame.bottom() - PAGE_BAR), frame.max));
    let view = Rect::from_min_max(
        frame.min,
        pos2(frame.right(), bar.map_or(frame.bottom(), |b| b.top())),
    );
    // Each page at the view's width, less the gaps and a scroll bar.
    let width = (view.width() - 2.0 * PAGE_GAP - ui.spacing().scroll.bar_width).max(1.0);
    let mut tops = Vec::with_capacity(pages);
    let mut y = PAGE_GAP;
    for [w, h] in sizes.iter() {
        tops.push(y);
        y += width * *h as f32 / *w as f32 + PAGE_GAP;
    }
    let height = |index: usize| width * sizes[index][1] as f32 / sizes[index][0] as f32;
    let jump_id = id.with("jump");
    let jump: Option<f32> = ctx.data_mut(|d| d.remove_temp(jump_id));
    let mut area = egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false]);
    if let Some(offset) = jump {
        area = area.vertical_scroll_offset(offset);
    }
    let mut shown = 0;
    ui.scope_builder(egui::UiBuilder::new().max_rect(view), |ui| {
        area.show_viewport(ui, |ui, viewport| {
            ui.set_min_size(vec2(view.width() - ui.spacing().scroll.bar_width, y));
            let origin = ui.max_rect().min;
            // The page whose part the view's middle shows; at the end the
            // last, which may be too short to reach the middle.
            let middle = viewport.center().y;
            shown = if viewport.bottom() >= y - 1.0 {
                pages - 1
            } else {
                tops.iter().rposition(|top| *top <= middle).unwrap_or(0)
            };
            for (index, top) in tops.iter().enumerate() {
                let (top, bottom) = (*top, top + height(index));
                if bottom < viewport.top() || top > viewport.bottom() {
                    continue;
                }
                let rect =
                    Rect::from_min_size(origin + vec2(PAGE_GAP, top), vec2(width, bottom - top));
                paint_page(ui, pictures, (data, index as u32), bytes, rect, scale, pass);
            }
        });
    });
    let Some(bar) = bar else {
        return;
    };
    // The page bar: previous, which page shows, next.
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(frame))
        .rect_filled(bar, 0.0, crate::theme::BAR_BG);
    let mut go = None;
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(bar.shrink2(vec2(4.0, 1.0))),
        |ui| {
            ui.horizontal_centered(|ui| {
                let previous = ui
                    .add_enabled(shown > 0, egui::Button::new("‹").frame(false))
                    .on_hover_text("Previous page");
                if previous.clicked() {
                    go = Some(shown - 1);
                }
                ui.label(format!("{} / {pages}", shown + 1))
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Label,
                            true,
                            format!("Page {} of {pages}", shown + 1),
                        )
                    });
                let next = ui
                    .add_enabled(shown + 1 < pages, egui::Button::new("›").frame(false))
                    .on_hover_text("Next page");
                if next.clicked() {
                    go = Some(shown + 1);
                }
            });
        },
    );
    if let Some(index) = go {
        // To the page's top, but no further than the pages' end: a short
        // last page is reached by scrolling to the end (which egui does not
        // clamp an offset given to it to).
        let end = (y - view.height()).max(0.0);
        let offset = (tops[index] - PAGE_GAP).clamp(0.0, end);
        ctx.data_mut(|d| d.insert_temp(jump_id, offset));
        ctx.request_repaint();
    }
}

/// Page `index` of the PDF view in `rect`: its drawing at the size it
/// shows, else the same page at another size meanwhile, else white.
fn paint_page(
    ui: &Ui,
    pictures: &mut Pictures,
    (data, index): ([u8; 16], u32),
    bytes: &mut dyn FnMut() -> Option<Arc<[u8]>>,
    rect: Rect,
    scale: f32,
    pass: u64,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::WHITE);
    let side = |points: f32| ((points * scale).round() as u32).clamp(1, MAX_TEXTURE_SIDE);
    let key = Key {
        data,
        frame: [side(rect.width()), side(rect.height())],
        format: 0,
        scale: scale.to_bits(),
        page: Some(index),
    };
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if let Some(texture) = pictures.textures.get_mut(&key) {
        texture.last = pass;
        painter.image(texture.handle.id(), rect, uv, Color32::WHITE);
        return;
    }
    if let Some(texture) = pictures
        .textures
        .iter_mut()
        .filter(|(kept, _)| kept.data == data && kept.page == Some(index))
        .map(|(_, texture)| texture)
        .max_by_key(|texture| texture.last)
    {
        texture.last = pass;
        painter.image(texture.handle.id(), rect, uv, Color32::WHITE);
    }
    if pictures.decoding.contains(&key)
        || pictures.failed_pages.contains(&key)
        || !pictures.may_start(pass)
    {
        return;
    }
    let Some(bytes) = bytes() else {
        return;
    };
    pictures.started.1 += 1;
    pictures.submit(
        ui.ctx(),
        Job {
            key,
            data: bytes,
            making: Making::Page(index),
            frame: [rect.width(), rect.height()],
            scale,
            format: GraphicFormat::default(),
        },
    );
}

/// A container's PDF data, whatever preview picture it holds too.
fn pdf_stream(container: &Container) -> Option<&[u8]> {
    container
        .streams
        .iter()
        .find(|(kind, bytes)| &kind.0 == b"PDF " && !bytes.is_empty())
        .map(|(_, bytes)| bytes.as_slice())
}

/// The PDF view of an interactive container field (#200) on its own, for
/// a host that holds the PDF's data: its pages scrolled through and paged
/// in a frame ([`paint_pdf_view`]), drawn by the PDF worker.
#[derive(Default)]
pub struct PdfViewer {
    pictures: Pictures,
    /// The data shown and its MD5.
    data: Option<(Arc<[u8]>, [u8; 16])>,
}

impl PdfViewer {
    /// Show `data`'s pages in `frame`.
    pub fn show(&mut self, ui: &mut Ui, data: &Arc<[u8]>, frame: Rect) {
        let md5 = match &self.data {
            Some((held, md5)) if Arc::ptr_eq(held, data) => *md5,
            _ => {
                let md5 = om_data::containers::md5([(b"".as_slice(), &data[..])]);
                self.data = Some((Arc::clone(data), md5));
                md5
            }
        };
        let held = Arc::clone(data);
        paint_pdf_view(
            ui,
            &mut self.pictures,
            md5,
            &mut || Some(Arc::clone(&held)),
            frame,
            egui::Id::unique(("pdf view", md5)),
        );
    }
}

/// Whether container data held by reference may be a PDF: a file whose
/// name ends in `.pdf`, as FileMaker types a PDF by its name.
fn named_pdf(name: &str) -> bool {
    name.len() > 4
        && name.is_char_boundary(name.len() - 4)
        && name[name.len() - 4..].eq_ignore_ascii_case(".pdf")
}

/// The data a container shows as a picture: a picture stream, else a PDF.
fn shown_stream(container: &Container) -> Option<(&[u8], bool)> {
    if let Some((_, bytes)) = om_data::containers::picture_stream(container) {
        return Some((bytes, false));
    }
    container
        .streams
        .iter()
        .find(|(kind, bytes)| &kind.0 == b"PDF " && !bytes.is_empty())
        .map(|(_, bytes)| (bytes.as_slice(), true))
}

/// Audio or video, which an interactive container plays: what Insert
/// Audio/Video stores (a `movie` path), or a file named as such
/// (OpenMaker's reading; FileMaker plays what its system plays).
pub(crate) fn is_media(path_type: Option<&str>, name: &str) -> bool {
    const KINDS: [&str; 18] = [
        "mp3", "m4a", "aac", "wav", "aif", "aiff", "flac", "oga", "ogg", "opus", "mp4", "m4v",
        "mov", "webm", "ogv", "mpg", "mpeg", "avi",
    ];
    path_type.is_some_and(|kind| kind.eq_ignore_ascii_case("movie"))
        || extension(name).is_some_and(|ext| KINDS.contains(&ext.as_str()))
}

/// A file name's extension: up to 8 ASCII letters and digits, lowercased;
/// nothing else of the name reaches a path.
fn extension(name: &str) -> Option<String> {
    let (_, ext) = name.rsplit_once('.')?;
    (!ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

/// Audio, which the player shows as a control bar rather than a picture.
/// An `.ogg` may hold video, so it is not taken for audio: a video element
/// plays audio alone too.
fn is_audio(name: &str) -> bool {
    const KINDS: [&str; 9] = [
        "mp3", "m4a", "aac", "wav", "aif", "aiff", "flac", "oga", "opus",
    ];
    extension(name).is_some_and(|ext| KINDS.contains(&ext.as_str()))
}

/// The page the player opens for a media copy named `file`, beside it in
/// the window's media folder: the engine's media element with its own
/// controls, starting by itself only with `autoplay` (the container's
/// "Start playback automatically", containers.md 6.6). Only the copy's
/// own name (hexadecimal and an extension of letters and digits) is in it.
fn player_page(file: &str, audio: bool, autoplay: bool) -> String {
    let (element, layout) = if audio {
        (
            "audio",
            "body{display:flex;align-items:center}audio{width:100%}",
        )
    } else {
        (
            "video",
            "body{background:#000}video{width:100%;height:100%}",
        )
    };
    let start = if autoplay { " autoplay" } else { "" };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         html,body{{margin:0;height:100%}}{layout}</style></head><body>\
         <{element} src=\"{file}\" controls preload=\"metadata\"{start}></{element}>\
         </body></html>"
    )
}

/// An interactive container's value when it is audio or video (#200): its
/// file's icon and name, under the window's player once its data is in a
/// file the player opens, whose page's address this gives (one that starts
/// by itself with `autoplay`); with `no_player`, the file and that note
/// saying why it does not play. `None` when the value is not media (or its
/// header is still read): [`paint`] shows it.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_media(
    ui: &mut Ui,
    pictures: &mut Pictures,
    calc: &Evaluator<'_>,
    value: &Container,
    frame: Rect,
    style: &ResolvedStyle,
    no_player: Option<&'static str>,
    autoplay: bool,
) -> Option<Option<String>> {
    let ctx = ui.ctx().clone();
    pictures.collect(&ctx);
    let name = value.text.as_str();
    let (data, unloaded) = match &value.library {
        Some(library) if value.is_unloaded() => {
            let info = calc.library_member(library)?.ok()??;
            is_media(Some(info.path_type.as_str()), name).then_some((library.md5, true))?
        }
        Some(library) => is_media(value.path_type, name).then_some((library.md5, false))?,
        None => is_media(value.path_type, name).then(|| {
            (
                om_data::containers::md5([(b"".as_slice(), value.data())]),
                false,
            )
        })?,
    };
    if !frame.is_positive() || !ui.is_rect_visible(frame) {
        return Some(None);
    }
    let note = no_player.or(
        matches!(pictures.media.files.get(&data), Some(MediaFile::Failed)).then_some(MEDIA_FAILED),
    );
    if let Some(note) = note {
        let band = 16.0_f32.min(frame.height() / 2.0);
        let above = Rect::from_min_max(frame.min, pos2(frame.right(), frame.bottom() - band));
        paint_file(ui, above, name, style);
        paint_note(
            ui,
            Rect::from_min_max(pos2(frame.left(), above.bottom()), frame.max),
            note,
        );
        return Some(None);
    }
    paint_file(ui, frame, name, style);
    match pictures.media.files.get(&data) {
        Some(MediaFile::Ready(pages)) => return Some(Some(pages[usize::from(autoplay)].clone())),
        Some(MediaFile::Writing | MediaFile::Failed) => return Some(None),
        None => {}
    }
    // A copy is one of the pass's reads, and only a few are written at once.
    let writing = pictures
        .media
        .files
        .values()
        .filter(|file| matches!(file, MediaFile::Writing))
        .count();
    if writing >= MEDIA_WRITERS || !pictures.may_start(ctx.cumulative_pass_nr()) {
        return Some(None);
    }
    pictures.started.1 += 1;
    let stored;
    let container = if unloaded {
        match calc.library_data(value.library.as_ref()?) {
            None | Some(Err(_)) => return Some(None),
            Some(Ok(data)) => {
                stored = data;
                &stored.data
            }
        }
    } else {
        value
    };
    pictures.media.write(&ctx, data, name, container.data());
    Some(None)
}

impl Media {
    /// Write `bytes` to a file of the window's media folder, on a thread
    /// of its own; [`Pictures::collect`] takes its address.
    fn write(&mut self, ctx: &egui::Context, data: [u8; 16], name: &str, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > MAX_MEDIA {
            self.files.insert(data, MediaFile::Failed);
            return;
        }
        if self.folder.is_none() {
            let mut folder = tempfile::Builder::new();
            folder.prefix("openmaker-media-");
            // Only its user may read the copies: the umask leaves a new
            // folder open to others.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                folder.permissions(std::fs::Permissions::from_mode(0o700));
            }
            match folder.tempdir() {
                Ok(folder) => self.folder = Some(Arc::new(folder)),
                Err(_) => {
                    self.files.insert(data, MediaFile::Failed);
                    return;
                }
            }
        }
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
        let kind = extension(name).unwrap_or_else(|| "bin".into());
        let file = format!("{hex}.{kind}");
        let path = folder.path().join(&file);
        let pages = [false, true].map(|autoplay| {
            let page = folder.path().join(format!(
                "{hex}{}.html",
                if autoplay { ".autoplay" } else { "" }
            ));
            (page, player_page(&file, is_audio(name), autoplay))
        });
        let (sender, _) = self.written.get_or_insert_with(mpsc::channel);
        let sender = sender.clone();
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("container-media".into())
            .spawn(move || {
                let written = std::fs::write(&path, &bytes).is_ok()
                    && pages
                        .iter()
                        .all(|(page, html)| std::fs::write(page, html).is_ok());
                let address = written.then(|| pages.map(|(page, _)| file_address(&page)));
                // Written: if the window closed meanwhile, its folder goes
                // now, before the address is sent.
                drop(folder);
                let _ = sender.send((data, address));
                ctx.request_repaint();
            });
        self.files.insert(
            data,
            if spawned.is_ok() {
                MediaFile::Writing
            } else {
                MediaFile::Failed
            },
        );
    }
}

/// A local file's `file:` address, its path percent-encoded.
fn file_address(path: &std::path::Path) -> String {
    let mut address = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            address.push(byte as char);
        } else {
            address.push_str(&format!("%{byte:02X}"));
        }
    }
    address
}

/// A one-line note in `band`, under a file that does not play: caption
/// text, cut short with an ellipsis where the field is narrow.
fn paint_note(ui: &Ui, band: Rect, note: &str) {
    let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(band));
    let mut job = egui::text::LayoutJob::simple(
        note.to_owned(),
        egui::FontId::proportional(11.0),
        crate::theme::CAPTION,
        band.width(),
    );
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('\u{2026}');
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(
        pos2(band.center().x, band.center().y - galley.size().y / 2.0),
        galley,
        crate::theme::CAPTION,
    );
}

/// Paint a container field's data in `frame`, the field's content box:
/// a picture, a file's icon and name, or nothing while it is read or when
/// it cannot be shown. `style` is the field's text style, for the name.
/// `view`, for an interactive container ([`is_interactive`]), shows a PDF
/// as the PDF view, its place kept under that id.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint(
    ui: &mut Ui,
    pictures: &mut Pictures,
    calc: &Evaluator<'_>,
    value: &Container,
    format: GraphicFormat,
    frame: Rect,
    style: &ResolvedStyle,
    view: Option<egui::Id>,
) {
    let ctx = ui.ctx().clone();
    pictures.collect(&ctx);
    // A field out of view reads, decodes and keeps nothing.
    if !frame.is_positive() || !ui.is_rect_visible(frame) {
        return;
    }
    let scale = ui.pixels_per_point()
        * ctx
            .layer_transform_to_global(ui.layer_id())
            .map_or(1.0, |t| t.scaling);
    let pass = ctx.cumulative_pass_nr();
    let name = value.text.as_str();
    // Data held by reference: its header says whether it is a picture.
    let (data, loaded) = match &value.library {
        Some(library) if value.is_unloaded() => {
            match calc.library_member(library) {
                None | Some(Err(_) | Ok(None)) => return,
                Some(Ok(Some(info))) => {
                    // Without a PDF worker (#566) a PDF shows as a file.
                    let worker = om_data::calc::functions::pdf_worker::available();
                    let pdf = info.path_type == "file" && named_pdf(name) && worker;
                    // An interactive container's PDF, also one FileMaker
                    // Pro stored with its preview (an `image` path,
                    // containers.md 6.2): its pages, once its data is read;
                    // its preview when it holds no PDF.
                    let viewed = view.filter(|_| {
                        worker
                            && named_pdf(name)
                            && matches!(info.path_type.as_str(), "file" | "image")
                    });
                    if let Some(id) = viewed {
                        match calc.library_data(library) {
                            None => return,
                            Some(Ok(stored)) if pdf_stream(&stored.data).is_some() => {
                                let mut bytes = || pdf_stream(&stored.data).map(Arc::from);
                                return paint_pdf_view(
                                    ui,
                                    pictures,
                                    library.md5,
                                    &mut bytes,
                                    frame,
                                    id,
                                );
                            }
                            Some(_) => {}
                        }
                    }
                    if !matches!(info.path_type.as_str(), "image" | "") && !pdf {
                        return paint_file(ui, frame, name, style);
                    }
                }
            }
            (library.md5, None)
        }
        Some(library) => (library.md5, Some(value)),
        None => match shown_stream(value) {
            Some((bytes, _)) => (
                om_data::containers::md5([(b"".as_slice(), bytes)]),
                Some(value),
            ),
            None => return paint_file(ui, frame, name, style),
        },
    };
    // An interactive container's PDF, before any preview picture it holds.
    if let (Some(id), Some(bytes)) = (view, loaded.and_then(pdf_stream)) {
        if om_data::calc::functions::pdf_worker::available() {
            let mut held = None;
            let mut bytes = || Some(Arc::clone(held.get_or_insert_with(|| Arc::from(bytes))));
            return paint_pdf_view(ui, pictures, data, &mut bytes, frame, id);
        }
    }
    match pictures.kinds.get(&data) {
        Some(Kind::File) => return paint_file(ui, frame, name, style),
        Some(Kind::Unreadable) => return,
        None => {}
    }
    let size = [frame.width(), frame.height()];
    let key = Key {
        data,
        frame: [
            (size[0] * scale).round() as u32,
            (size[1] * scale).round() as u32,
        ],
        format: format.0,
        scale: scale.to_bits(),
        page: None,
    };
    if let Some(texture) = pictures.textures.get_mut(&key) {
        texture.last = pass;
        draw(ui, frame, format, scale, texture);
        return;
    }
    // Meanwhile the same picture at another size, when one is kept whole.
    if let Some(texture) = pictures
        .textures
        .iter_mut()
        .filter(|(kept, texture)| {
            kept.data == data && kept.page.is_none() && texture.source == [0.0, 0.0, 1.0, 1.0]
        })
        .map(|(_, texture)| texture)
        .max_by_key(|texture| texture.last)
    {
        texture.last = pass;
        draw(ui, frame, format, scale, texture);
    }
    if pictures.decoding.contains(&key) || !pictures.may_start(pass) {
        return;
    }
    let stored;
    let container = match loaded {
        Some(container) => container,
        None => {
            pictures.started.1 += 1;
            let Some(library) = &value.library else {
                return;
            };
            match calc.library_data(library) {
                None | Some(Err(_)) => return,
                Some(Ok(data)) => {
                    stored = data;
                    &stored.data
                }
            }
        }
    };
    let Some((bytes, pdf)) = shown_stream(container)
        .filter(|(_, pdf)| !pdf || om_data::calc::functions::pdf_worker::available())
    else {
        pictures.remember(data, Kind::File);
        return paint_file(ui, frame, name, style);
    };
    pictures.submit(
        &ctx,
        Job {
            key,
            data: Arc::from(bytes),
            making: if pdf {
                Making::FirstPage
            } else {
                Making::Picture
            },
            frame: size,
            scale,
            format,
        },
    );
}

/// The texture's part of the picture where the format places it, clipped
/// to the frame.
fn draw(ui: &Ui, frame: Rect, format: GraphicFormat, scale: f32, texture: &Texture) {
    let natural = texture.natural;
    let [x, y, w, h] = placement(format, natural, [frame.width(), frame.height()], scale);
    let [left, top, right, bottom] = texture.source;
    if right <= left || bottom <= top {
        return;
    }
    let rect = Rect::from_min_max(
        frame.min + vec2(x + left * w, y + top * h),
        frame.min + vec2(x + right * w, y + bottom * h),
    );
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(frame))
        .image(
            texture.handle.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
}

/// A file: a document icon above its name, centered in the frame (the
/// arrangement is FileMaker's; the icon is OpenMaker's own, not the
/// system's icon for the file's type).
fn paint_file(ui: &mut Ui, frame: Rect, name: &str, style: &ResolvedStyle) {
    let mut style = style.clone();
    style.text_align = Some(2);
    style.vertical_align = Some(0);
    let line = style.font_size.unwrap_or(12.0) as f32 * 1.3;
    let text = if name.is_empty() { 0.0 } else { line };
    let icon = FILE_ICON
        .min(frame.height() - text - 4.0)
        .min(frame.width());
    let color = style.color.map_or(crate::theme::ICON, super::color);
    let top = frame.top() + ((frame.height() - icon.max(0.0) - 2.0 - text) / 2.0).max(0.0);
    if icon >= 8.0 {
        let rect =
            Rect::from_center_size(pos2(frame.center().x, top + icon / 2.0), vec2(icon, icon));
        icons::paint(ui.painter(), rect, Icon::File, color, Color32::TRANSPARENT);
    }
    if !name.is_empty() {
        let text_top = if icon >= 8.0 { top + icon + 2.0 } else { top };
        let rect = Rect::from_min_max(pos2(frame.left(), text_top), frame.max);
        super::paint_text(ui, rect, name, &style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio and video by Insert Audio/Video's path or the file's name;
    /// only the name's extension, letters and digits, reaches the copy's
    /// path, whatever else the name holds.
    #[test]
    fn media_is_told_by_its_path_or_name_and_only_its_extension_is_kept() {
        assert!(is_media(Some("movie"), "clip"));
        assert!(is_media(Some("file"), "Talk.MP3") && is_media(None, "a.b.webm"));
        assert!(!is_media(Some("file"), "notes.txt") && !is_media(Some("image"), "x.png"));
        assert_eq!(extension("../../etc/x.Mp4").as_deref(), Some("mp4"));
        assert_eq!(extension("a.mp4/../../b"), None);
        assert_eq!(extension("no extension"), None);
        assert_eq!(
            file_address(std::path::Path::new("/tmp/a b/é#.mp4")),
            "file:///tmp/a%20b/%C3%A9%23.mp4"
        );
        assert!(is_audio("Talk.MP3") && is_audio("talk.oga"));
        assert!(!is_audio("clip.mp4") && !is_audio("clip.ogg") && !is_audio("clip"));
    }

    /// The player's page plays the copy beside it with the engine's own
    /// controls, and starts by itself only with "Start playback
    /// automatically" (#1338).
    #[test]
    fn the_player_page_starts_by_itself_only_when_asked() {
        let video = player_page("00ff.mp4", false, false);
        assert!(
            video.contains("<video src=\"00ff.mp4\" controls"),
            "{video}"
        );
        assert!(!video.contains("autoplay"), "{video}");
        let started = player_page("00ff.mp4", false, true);
        assert!(started.contains(" autoplay></video>"), "{started}");
        let audio = player_page("00ff.mp3", true, true);
        assert!(
            audio.contains("<audio src=\"00ff.mp3\" controls"),
            "{audio}"
        );
        assert!(audio.contains(" autoplay></audio>"), "{audio}");
    }

    /// The player's copy is the data, in a folder only its user may read,
    /// gone with the window.
    #[cfg(unix)]
    #[test]
    fn a_media_copy_is_private_and_goes_with_the_window() {
        use std::os::unix::fs::PermissionsExt;
        let ctx = egui::Context::default();
        let mut media = Media::default();
        media.write(&ctx, [7; 16], "../clip.mp4", b"frames");
        let (_, written) = media.written.as_ref().unwrap();
        let (data, pages) = written
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(data, [7; 16]);
        let folder = media.folder.as_ref().unwrap().path().to_owned();
        let hex = "07".repeat(16);
        let path = folder.join(format!("{hex}.mp4"));
        assert_eq!(std::fs::read(&path).unwrap(), b"frames");
        // A page to wait for the play control, one to start by itself.
        let page = |name: String| folder.join(name);
        assert_eq!(
            pages,
            Some([
                file_address(&page(format!("{hex}.html"))),
                file_address(&page(format!("{hex}.autoplay.html"))),
            ])
        );
        let started = std::fs::read_to_string(page(format!("{hex}.autoplay.html"))).unwrap();
        assert!(started.contains(&format!("src=\"{hex}.mp4\"")) && started.contains("autoplay"));
        let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        drop(media);
        assert!(!folder.exists());
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_fn(width, height, |x, _| {
            image::Rgba(if x < width / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            })
        })
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        bytes.into_inner()
    }

    /// A 400 x 100 picture in a 100 x 100 frame at 2 pixels a point:
    /// reduced, the whole picture at its shown size (200 x 50 pixels);
    /// cropped and centered, its middle 100 x 100 pixels.
    #[test]
    fn decodes_the_part_shown_at_its_shown_size() {
        let data = png(400, 100);
        let reduced = decode(&data, [100.0, 100.0], 2.0, GraphicFormat(7)).unwrap();
        assert_eq!(reduced.natural, [400, 100]);
        assert_eq!(reduced.source, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(reduced.image.size, [200, 50]);
        let cropped = decode(&data, [100.0, 100.0], 2.0, GraphicFormat(0)).unwrap();
        assert_eq!(cropped.source, [0.375, 0.0, 0.625, 1.0]);
        assert_eq!(cropped.image.size, [100, 100]);
        assert_eq!(cropped.image.pixels[0], Color32::RED);
        assert_eq!(cropped.image.pixels[99], Color32::BLUE);
    }

    /// A file whose name ends in `.pdf` may be a PDF; its first page is
    /// drawn through the PDF worker (`tests/browse_containers.rs`).
    #[test]
    fn pdf_names() {
        assert!(named_pdf("Report.PDF") && !named_pdf(".pdf") && !named_pdf("a.pdfx"));
    }

    /// Hostile data: truncated, oversized and unknown pictures do not
    /// decode, and nothing panics.
    #[test]
    fn refuses_data_that_does_not_decode_within_the_limits() {
        let data = png(40, 10);
        assert!(decode(&data[..data.len() / 2], [10.0, 10.0], 1.0, GraphicFormat(5)).is_none());
        assert!(decode(b"not a picture", [10.0, 10.0], 1.0, GraphicFormat(5)).is_none());
        // A PNG header claiming 100,000 x 100,000 pixels.
        let mut huge = data.clone();
        huge[16..24].copy_from_slice(&[0, 1, 0x86, 0xa0, 0, 1, 0x86, 0xa0]);
        assert!(decode(&huge, [10.0, 10.0], 1.0, GraphicFormat(5)).is_none());
    }
}
