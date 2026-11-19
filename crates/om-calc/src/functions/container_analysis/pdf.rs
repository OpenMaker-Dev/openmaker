//! PDF data: the first page's size, the text of every page
//! (`GetTextFromPDF`) and a picture of the first page (`GetThumbnail`).
//!
//! The text follows FileMaker Server's extraction (oracle): page by page,
//! text in content-stream order, cut into lines where the baseline moves
//! by more than 0.7 em (8.4 points at 12 points: 8 stays on the line, 9
//! breaks it); a line's pieces are put in left-to-right order, a piece
//! that repeats the same text at the same place is dropped, and a space
//! separates pieces that leave a gap wider than 0.15 em or overlap. Runs of
//! spaces become one space (at the ends of a line too), tabs stay, lines
//! and pages are joined with returns, pages without text are skipped and
//! nothing ends the text. Invisible text (render mode 3, what OCR layers
//! use) counts; the crop box does not clip.

use std::sync::Arc;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask,
};
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

fn open(data: &[u8]) -> Option<Pdf> {
    Pdf::new(Arc::new(data.to_vec())).ok()
}

/// The first page's size in points as FileMaker reports it (the crop box,
/// turned by the page's rotation, truncated).
pub(super) fn page_size(data: &[u8]) -> Option<(u32, u32)> {
    let pdf = open(data)?;
    let page = pdf.pages().first()?;
    let (w, h) = page.render_dimensions();
    Some((w as u32, h as u32))
}

/// Every page's size in points, as [`page_size`] gives the first's, for at
/// most `limit` pages; `None` when the data is not a PDF.
pub(super) fn page_sizes(data: &[u8], limit: usize) -> Option<Vec<(u32, u32)>> {
    let pdf = open(data)?;
    Some(
        pdf.pages()
            .iter()
            .take(limit)
            .map(|page| {
                let (w, h) = page.render_dimensions();
                (w as u32, h as u32)
            })
            .collect(),
    )
}

/// The first page drawn on white at `width` x `height` pixels.
pub(super) fn render(data: &[u8], width: u32, height: u32) -> Option<image::RgbImage> {
    render_page(data, 0, width, height)
}

/// Page `index` (from 0) drawn on white at `width` x `height` pixels.
pub(super) fn render_page(
    data: &[u8],
    index: u32,
    width: u32,
    height: u32,
) -> Option<image::RgbImage> {
    let pdf = open(data)?;
    let page = pdf.pages().get(index as usize)?;
    let (w, h) = page.render_dimensions();
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: width as f32 / w,
            y_scale: height as f32 / h,
            bg_color: WHITE,
        },
    );
    let (pw, ph) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    let rgb: Vec<u8> = pixmap
        .data_as_u8_slice()
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let picture = image::RgbImage::from_raw(pw, ph, rgb)?;
    Some(if (pw, ph) == (width, height) {
        picture
    } else {
        image::imageops::resize(
            &picture,
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
    })
}

/// The text of every page; `None` when the data is not a PDF.
pub(super) fn text(data: &[u8]) -> Option<String> {
    let pdf = open(data)?;
    let cache = InterpreterCache::new();
    let mut pages = Vec::new();
    for page in pdf.pages().iter() {
        let mut glyphs = Glyphs::default();
        let mut context = Context::new(
            Affine::IDENTITY,
            Rect::new(-1e9, -1e9, 1e9, 1e9),
            &cache,
            page.xref(),
            InterpreterSettings::default(),
        );
        interpret_page(page, &mut context, &mut glyphs);
        let text = layout(&glyphs.0);
        if !text.is_empty() {
            pages.push(text);
        }
    }
    Some(pages.join("\r"))
}

/// Literal, case-sensitive, non-overlapping matches on one page. This
/// uses the same extraction and spacing rules as `text`; MBS's matching
/// and ordering are unmeasured (docs/re/mbs-plugin.md 5).
pub(super) fn find(data: &[u8], page: u32, pattern: &str) -> Option<u32> {
    u32::try_from(page_text(data, page)?.matches(pattern).count()).ok()
}

/// The existing extraction of one page, also used to check highlight counts.
pub(super) fn page_text(data: &[u8], page: u32) -> Option<String> {
    let pdf = open(data)?;
    let page = pdf.pages().get(page as usize)?;
    let mut glyphs = Glyphs::default();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        Affine::IDENTITY,
        Rect::new(-1e9, -1e9, 1e9, 1e9),
        &cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    interpret_page(page, &mut context, &mut glyphs);
    Some(layout(&glyphs.0))
}

/// A glyph drawn on the page, in page space.
#[derive(Clone, Debug)]
pub(super) struct Placed {
    pub(super) text: String,
    pub(super) x0: f64,
    pub(super) x1: f64,
    pub(super) y: f64,
    pub(super) size: f64,
}

#[derive(Default)]
struct Glyphs(Vec<Placed>);

impl<'a> Device<'a> for Glyphs {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {
        for g in run.glyphs() {
            // Glyph space has 1000 units to the em.
            let t = g.transform();
            let origin = t * Point::ORIGIN;
            let advance = match &**g {
                Glyph::Outline(o) => o.advance_width().unwrap_or(0.0),
                Glyph::Type3(_) => 0.0,
            };
            let end = t * Point::new(f64::from(advance), 0.0);
            let size = (t * Point::new(0.0, 1000.0) - origin).hypot();
            let text = match g.as_unicode() {
                Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
                Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => s,
                None => String::new(),
            };
            self.0.push(Placed {
                text: expand_ligatures(&text),
                x0: origin.x.min(end.x),
                x1: origin.x.max(end.x),
                y: origin.y,
                size,
            });
        }
    }
}

/// Ligature characters as their letters (`ﬁ` is `fi`).
pub(super) fn expand_ligatures(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            '\0' => {}
            c => out.push(c),
        }
    }
    out
}

/// A run of adjacent glyphs on a line.
struct Piece {
    text: String,
    x0: f64,
    x1: f64,
    size: f64,
}

/// A space between two glyphs or pieces: a gap wider than 0.15 em, or an
/// overlap of more than 0.3 em (text drawn back over earlier text).
fn separated(x1: f64, x0: f64, size: f64) -> bool {
    x0 - x1 > 0.15 * size || x0 < x1 - 0.3 * size
}

fn push_space(text: &mut String) {
    if !text.is_empty() && !text.ends_with(' ') {
        text.push(' ');
    }
}

pub(super) fn layout(glyphs: &[Placed]) -> String {
    let mut lines: Vec<(f64, f64, Vec<Piece>)> = Vec::new();
    for g in glyphs {
        let new_line = match lines.last() {
            Some((y, size, _)) => (g.y - y).abs() > 0.7 * size.max(g.size),
            None => true,
        };
        if new_line {
            lines.push((g.y, g.size, Vec::new()));
        }
        let pieces = &mut lines.last_mut().expect("a line").2;
        match pieces.last_mut() {
            Some(p) if g.x0 >= p.x1 - 0.3 * p.size.max(g.size) => {
                if g.x0 - p.x1 > 0.15 * p.size.max(g.size) && !g.text.starts_with(' ') {
                    push_space(&mut p.text);
                }
                p.text.push_str(&g.text);
                p.x1 = p.x1.max(g.x1);
                p.size = p.size.max(g.size);
            }
            _ => pieces.push(Piece {
                text: g.text.clone(),
                x0: g.x0,
                x1: g.x1,
                size: g.size,
            }),
        }
    }
    let mut out = Vec::new();
    for (_, _, mut pieces) in lines {
        pieces.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut kept: Vec<Piece> = Vec::new();
        for p in pieces {
            let repeat = kept
                .iter()
                .any(|k| k.text == p.text && (k.x0 - p.x0).abs() < 0.5 * k.size.max(p.size));
            if !repeat {
                kept.push(p);
            }
        }
        let mut line = String::new();
        let mut last: Option<(f64, f64)> = None;
        for p in &kept {
            if let Some((x1, size)) = last {
                if separated(x1, p.x0, size.max(p.size)) && !p.text.starts_with(' ') {
                    push_space(&mut line);
                }
            }
            line.push_str(&p.text);
            last = Some((p.x1, p.size));
        }
        let mut collapsed = String::with_capacity(line.len());
        for c in line.chars() {
            if !(c == ' ' && collapsed.ends_with(' ')) {
                collapsed.push(c);
            }
        }
        if !collapsed.is_empty() {
            out.push(collapsed);
        }
    }
    out.join("\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(text: &str, x0: f64, y: f64) -> Placed {
        Placed {
            text: text.into(),
            x0,
            x1: x0 + 6.0 * text.chars().count() as f64,
            y,
            size: 12.0,
        }
    }

    #[test]
    fn lines_follow_the_stream_and_pieces_the_page() {
        // A line drawn after the one below it stays after it.
        assert_eq!(
            layout(&[g("Bottom", 10.0, 50.0), g("Top", 10.0, 150.0)]),
            "Bottom\rTop"
        );
        // Pieces of a line are sorted left to right.
        assert_eq!(
            layout(&[g("World", 100.0, 150.0), g("Hello", 10.0, 150.0)]),
            "Hello World"
        );
        // A baseline 8 points lower is the same line, 9 is a new one.
        assert_eq!(
            layout(&[g("Up", 10.0, 150.0), g("Down", 30.0, 142.0)]),
            "Up Down"
        );
        assert_eq!(
            layout(&[g("Up", 10.0, 150.0), g("Down", 30.0, 141.0)]),
            "Up\rDown"
        );
        // The same text at the same place once.
        assert_eq!(
            layout(&[g("Same", 10.0, 150.0), g("Same", 10.5, 150.5)]),
            "Same"
        );
        // Overlapping text is separated.
        assert_eq!(
            layout(&[g("Same", 10.0, 150.0), g("Diff", 10.0, 150.0)]),
            "Same Diff"
        );
        // Adjacent glyphs join, a gap of 2 points at 12 points separates.
        assert_eq!(
            layout(&[g("ab", 10.0, 150.0), g("cd", 23.5, 150.0)]),
            "abcd"
        );
        assert_eq!(
            layout(&[g("ab", 10.0, 150.0), g("cd", 24.0, 150.0)]),
            "ab cd"
        );
        // Spaces collapse, at the ends too.
        assert_eq!(layout(&[g("  a  b  ", 10.0, 150.0)]), " a b ");
    }
}
