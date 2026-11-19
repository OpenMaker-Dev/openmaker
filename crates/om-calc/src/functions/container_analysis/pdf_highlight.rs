//! Bounded highlight positions for horizontal outline text. MBS documents
//! counts and annotations, not its matching or padding; those are inferred
//! (docs/re/mbs-plugin.md 5). Never return a partial set: compare against
//! extraction offsets and refuse text split in a way we cannot locate.
use super::{
    pdf,
    pdf_form::{rect_ok, MAX_HIGHLIGHTS},
};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask,
};
use hayro::kurbo::{Affine, BezPath, Point, Rect, Shape};
use std::sync::Arc;

pub(super) fn positions(
    data: &[u8],
    page: u32,
    pattern: &str,
    insensitive: bool,
) -> Option<Vec<[f64; 4]>> {
    let text = pdf::page_text(data, page)?;
    let (text, pattern) = if insensitive {
        (text.to_lowercase(), pattern.to_lowercase())
    } else {
        (text, pattern.to_owned())
    };
    let count = text.matches(&pattern).count();
    if count > MAX_HIGHLIGHTS {
        return None;
    }
    let pdf = Pdf::new(Arc::new(data.to_vec())).ok()?;
    let page = pdf.pages().get(page as usize)?;
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        Affine::IDENTITY,
        Rect::new(-1e9, -1e9, 1e9, 1e9),
        &cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut device = Matches {
        pattern: &pattern,
        page_text: &text,
        match_ranges: Vec::new(),
        insensitive,
        rects: Vec::new(),
        failed: false,
    };
    interpret_page(page, &mut context, &mut device);
    device.match_ranges.sort_unstable_by_key(|r| r.start);
    let expected: Vec<_> = text
        .match_indices(&pattern)
        .map(|(at, s)| at..at + s.len())
        .collect();
    (!device.failed && device.match_ranges == expected).then_some(device.rects)
}

struct Matches<'a> {
    pattern: &'a str,
    page_text: &'a str,
    match_ranges: Vec<std::ops::Range<usize>>,
    insensitive: bool,
    rects: Vec<[f64; 4]>,
    failed: bool,
}
impl<'a> Device<'a> for Matches<'_> {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {
        if self.failed {
            return;
        }
        let mut text = String::new();
        let mut glyphs = Vec::new();
        let mut placed = Vec::new();
        for glyph in run.glyphs() {
            let t = glyph.transform();
            let [a, b, c, d, _, _] = t.as_coeffs();
            if b.abs() > 1e-8 || c.abs() > 1e-8 || a <= 0.0 || d <= 0.0 {
                self.failed = true;
                return;
            }
            let Some(unicode) = glyph.as_unicode() else {
                self.failed = true;
                return;
            };
            let unicode = match unicode {
                hayro::hayro_interpret::hayro_cmap::BfString::Char(c) => c.to_string(),
                hayro::hayro_interpret::hayro_cmap::BfString::String(s) => s,
            };
            let unicode = pdf::expand_ligatures(&unicode);
            let start = text.len();
            if self.insensitive {
                text.push_str(&unicode.to_lowercase());
            } else {
                text.push_str(&unicode);
            }
            let Glyph::Outline(outline) = &**glyph else {
                self.failed = true;
                return;
            };
            let origin = t * Point::ORIGIN;
            let end = t * Point::new(f64::from(outline.advance_width().unwrap_or(0.0)), 0.0);
            placed.push(pdf::Placed {
                text: if self.insensitive {
                    unicode.to_lowercase()
                } else {
                    unicode
                },
                x0: origin.x.min(end.x),
                x1: origin.x.max(end.x),
                y: origin.y,
                size: (t * Point::new(0.0, 1000.0) - origin).hypot(),
            });
            let mut outline = outline.outline();
            outline.apply_affine(t);
            let rect = outline.bounding_box();
            glyphs.push((start..text.len(), rect));
        }
        // A count can accidentally balance a missed match across runs
        // against a false raw-glyph match across a large positioning gap.
        if text.contains(self.pattern) && pdf::layout(&placed) != text {
            self.failed = true;
            return;
        }
        if !text.contains(self.pattern) {
            return;
        }
        // Counts alone cannot distinguish (A) Tj (AA) Tj from a match
        // at the start of the extracted AAA. Locate this entire run
        // unambiguously, including overlapping occurrences, then compare
        // every match offset with the extractor's global match ranges.
        let Some(offset) = self.page_text.find(&text) else {
            self.failed = true;
            return;
        };
        let next = offset
            + text
                .chars()
                .next()
                .expect("a matching run is nonempty")
                .len_utf8();
        if self.page_text[next..].contains(&text) {
            self.failed = true;
            return;
        }
        for (at, matched) in text.match_indices(self.pattern) {
            let end = at + matched.len();
            let mut bounds: Option<Rect> = None;
            for (range, rect) in &glyphs {
                if range.start < end && range.end > at && rect.area() > 0.0 {
                    bounds = Some(bounds.map_or(*rect, |b| b.union(*rect)));
                }
            }
            let Some(r) = bounds else {
                self.failed = true;
                return;
            };
            let rect = [r.x0, r.y0, r.x1, r.y1];
            if !rect_ok(rect) || self.rects.len() == MAX_HIGHLIGHTS {
                self.failed = true;
                return;
            }
            // The extractor deduplicates overprinted text. Refuse
            // overlapping locations instead of letting duplicate glyph
            // matches compensate for one we could not locate elsewhere.
            if self.rects.iter().any(|old| {
                let old = Rect::new(old[0], old[1], old[2], old[3]);
                old.intersect(r).area() > 0.5 * old.area().min(r.area())
            }) {
                self.failed = true;
                return;
            }
            self.match_ranges.push(offset + at..offset + end);
            self.rects.push(rect);
        }
    }
}
