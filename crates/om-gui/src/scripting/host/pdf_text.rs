//! The text the plug-in subset's DynaPDF commands write
//! (`om_scripts::runtime::dynapdf_text`, docs/plugins.md), laid out and
//! drawn as a PDF of a page for each page written on, which the PDF worker
//! draws over the document's pages. It is set in the faces reports embed
//! (`crate::report`): Liberation Sans, metric-compatible with Arial and
//! Helvetica.
//!
//! A block is laid out in its rectangle, in bottom-up coordinates:
//! - lines break at each line break and, between words, where the next word
//!   would pass the rectangle's width; a line wider than the width (a word
//!   longer than it, or the spaces before a word) is refused [I: whether
//!   DynaPDF breaks it or lets it pass the rectangle is not documented];
//!   a block whose ink must fit (a button's caption) is refused too when
//!   its glyphs' outlines are wider or taller than its rectangle;
//! - each line is aligned left, center or right;
//! - the first line's top is the rectangle's top;
//! - each line is spaced by its faces' ascent less their descent, never
//!   less than the font size [I: the DynaPDF manual's rule for `\LD`
//!   without typographic metrics; its default leading is not documented],
//!   nor so little that its ascent reaches into the line above's descent
//!   [I: OpenMaker's choice, for a line smaller than the one above].
//!
//! Text that passes a limited rectangle's height is refused: DynaPDF would
//! continue it on another page, which OpenMaker does not make. So is a
//! character the face does not have.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use om_data::runtime::dynapdf_text::{Align, Face, TextBlock};
use pdf_writer::{Finish, Name, Pdf, Rect, Ref};

use om_data::runtime::{ErrorCode, HostError};

use crate::report::{embed, ReportFace};

/// Why a block is not drawn.
#[derive(Debug, PartialEq)]
pub(crate) enum Refusal {
    /// Its text passes its rectangle's height.
    Overflow,
    /// A line of it (a word longer than the width, or the spaces before a
    /// word) passes its width, or, for a block whose ink must fit, its
    /// glyphs' outlines do.
    TooWide,
    /// Its face has no glyph for this character.
    Glyph(char),
}

impl Refusal {
    /// The host error for it: 3 for text that does not fit, 800 for a
    /// missing glyph (`om_scripts::runtime::host::Files::check_pdf_text`).
    pub(crate) fn error(self) -> HostError {
        match self {
            Refusal::Overflow => ErrorCode(3).into(),
            Refusal::TooWide => ErrorCode(4).into(),
            Refusal::Glyph(_) => ErrorCode(800).into(),
        }
    }
}

fn report_face(face: Face) -> ReportFace {
    match face {
        Face::Sans => ReportFace::Sans,
        Face::SansBold => ReportFace::SansBold,
    }
}

/// One glyph as laid out: its face, size, color, glyph id, advance in
/// points, and the character it shows.
#[derive(Clone, Copy)]
struct Glyph {
    face: ReportFace,
    size: f64,
    color: [f64; 3],
    id: u16,
    advance: f64,
    ch: char,
}

/// A face's ascent and descent per point of size (descent negative).
fn extents(face: ReportFace) -> (f64, f64) {
    let f = face.face();
    let em = f64::from(f.units_per_em());
    (f64::from(f.ascender()) / em, f64::from(f.descender()) / em)
}

/// A line's glyphs, and its ascent, descent and leading in points.
struct Line {
    glyphs: Vec<Glyph>,
    ascent: f64,
    descent: f64,
    leading: f64,
}

impl Line {
    /// A line of `glyphs`; an empty one takes its metrics from `style`.
    fn new(mut glyphs: Vec<Glyph>, style: (ReportFace, f64)) -> Line {
        while glyphs.last().is_some_and(|g| g.ch == ' ') {
            glyphs.pop();
        }
        let metrics: Vec<(ReportFace, f64)> = if glyphs.is_empty() {
            vec![style]
        } else {
            glyphs.iter().map(|g| (g.face, g.size)).collect()
        };
        let (mut ascent, mut descent, mut leading) = (0.0f64, 0.0f64, 0.0f64);
        for (face, size) in metrics {
            let (a, d) = extents(face);
            ascent = ascent.max(a * size);
            descent = descent.min(d * size);
            leading = leading.max(((a - d) * size).max(size));
        }
        Line {
            glyphs,
            ascent,
            descent,
            leading,
        }
    }

    fn width(&self) -> f64 {
        self.glyphs.iter().map(|g| g.advance).sum()
    }
}

/// Lines as they are wrapped: those made, the one being filled and its
/// width, and the word being read and its width.
#[derive(Default)]
struct Wrap {
    lines: Vec<Line>,
    current: Vec<Glyph>,
    current_width: f64,
    has_word: bool,
    word: Vec<Glyph>,
    word_width: f64,
}

impl Wrap {
    /// The word goes on the line, or starts the next when it would pass
    /// `width` and the line has a word already.
    fn place(&mut self, width: f64, style: (ReportFace, f64)) {
        if self.word.is_empty() {
            return;
        }
        if self.has_word && self.current_width + self.word_width > width + 1e-6 {
            self.end_line(style);
        }
        self.current.append(&mut self.word);
        self.current_width += std::mem::take(&mut self.word_width);
        self.has_word = true;
    }

    fn end_line(&mut self, style: (ReportFace, f64)) {
        self.lines
            .push(Line::new(std::mem::take(&mut self.current), style));
        self.current_width = 0.0;
        self.has_word = false;
    }
}

/// The block's lines, wrapped at its width.
fn lines(block: &TextBlock) -> Result<Vec<Line>, Refusal> {
    let mut wrap = Wrap::default();
    let mut style = block
        .spans
        .first()
        .map_or((ReportFace::Sans, 12.0), |s| (report_face(s.face), s.size));
    for span in &block.spans {
        let face = report_face(span.face);
        let ttf = face.face();
        let scale = span.size / f64::from(ttf.units_per_em());
        style = (face, span.size);
        for ch in span.text.chars() {
            if ch == '\n' {
                wrap.place(block.width, style);
                wrap.end_line(style);
                continue;
            }
            let id = ttf.glyph_index(ch).ok_or(Refusal::Glyph(ch))?;
            let advance = f64::from(ttf.glyph_hor_advance(id).unwrap_or(0)) * scale;
            let glyph = Glyph {
                face,
                size: span.size,
                color: span.color,
                id: id.0,
                advance,
                ch,
            };
            if ch == ' ' {
                wrap.place(block.width, style);
                wrap.current.push(glyph);
                wrap.current_width += advance;
            } else {
                wrap.word.push(glyph);
                wrap.word_width += advance;
            }
        }
    }
    wrap.place(block.width, style);
    if !wrap.current.is_empty() || wrap.lines.is_empty() {
        wrap.end_line(style);
    }
    Ok(wrap.lines)
}

/// The block's lines and each one's baseline; refused when they pass its
/// rectangle's height.
fn placed(block: &TextBlock) -> Result<Vec<(Line, f64)>, Refusal> {
    let lines = lines(block)?;
    let mut placed: Vec<(Line, f64)> = Vec::with_capacity(lines.len());
    for line in lines {
        let baseline = match placed.last() {
            None => block.top - line.ascent,
            Some((above, at)) => at - line.leading.max(line.ascent - above.descent),
        };
        let bottom = baseline + line.descent;
        if block.height.is_some_and(|h| block.top - bottom > h + 0.01) {
            return Err(Refusal::Overflow);
        }
        if line.width() > block.width + 0.01 {
            return Err(Refusal::TooWide);
        }
        placed.push((line, baseline));
    }
    Ok(placed)
}

/// Lay `block` out as [`render`] would, drawing nothing. A block whose ink
/// must fit is measured as drawn: the box its page gets, outlines
/// included, which a button's appearance centers in the button.
pub(crate) fn check(block: &TextBlock) -> Result<(), Refusal> {
    placed(block)?;
    if block.ink_inside {
        let [x0, y0, x1, y1] = draw(std::slice::from_ref(block), &mut BTreeMap::new())?.bounds;
        if x1 - x0 > block.width + 0.01 {
            return Err(Refusal::TooWide);
        }
        if block.height.is_some_and(|h| y1 - y0 > h + 0.01) {
            return Err(Refusal::Overflow);
        }
    }
    Ok(())
}

/// A page's content stream and its box around what it draws.
struct Drawn {
    content: String,
    bounds: [f64; 4],
}

/// The content of a page of `blocks`, the glyphs it shows added to `maps`.
fn draw(
    blocks: &[TextBlock],
    maps: &mut BTreeMap<ReportFace, BTreeMap<u16, char>>,
) -> Result<Drawn, Refusal> {
    let mut content = String::new();
    // The box around what is drawn, and only that: the lines' advances,
    // ascents and descents, and every glyph's outline, which may reach past
    // them (a `j`'s tail); the worker draws the page as a form clipped to
    // it, and a button centers its caption by it. A page that draws nothing
    // gets a unit box.
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for block in blocks {
        for (line, baseline) in placed(block)? {
            let bottom = baseline + line.descent;
            let width = line.width();
            let mut x = block.left
                + match block.align {
                    Align::Left => 0.0,
                    Align::Center => (block.width - width) / 2.0,
                    Align::Right => block.width - width,
                };
            x0 = x0.min(x);
            x1 = x1.max(x + width);
            y0 = y0.min(bottom);
            y1 = y1.max(baseline + line.ascent);
            let mut start = 0;
            while start < line.glyphs.len() {
                let first = line.glyphs[start];
                let end = line.glyphs[start..]
                    .iter()
                    .position(|g| {
                        g.face != first.face || g.size != first.size || g.color != first.color
                    })
                    .map_or(line.glyphs.len(), |n| start + n);
                let run = &line.glyphs[start..end];
                let [r, g, b] = first.color;
                let (name, _) = first.face.names();
                let _ = write!(
                    content,
                    "BT /{} {} Tf {r:.4} {g:.4} {b:.4} rg 1 0 0 1 {x:.3} {baseline:.3} Tm <",
                    String::from_utf8_lossy(name),
                    first.size
                );
                let map = maps.entry(first.face).or_default();
                let mut pen = x;
                for glyph in run {
                    let _ = write!(content, "{:04X}", glyph.id);
                    map.insert(glyph.id, glyph.ch);
                    let face = glyph.face.face();
                    let scale = glyph.size / f64::from(face.units_per_em());
                    if let Some(b) = face.glyph_bounding_box(ttf_parser::GlyphId(glyph.id)) {
                        x0 = x0.min(pen + f64::from(b.x_min) * scale);
                        x1 = x1.max(pen + f64::from(b.x_max) * scale);
                        y0 = y0.min(baseline + f64::from(b.y_min) * scale);
                        y1 = y1.max(baseline + f64::from(b.y_max) * scale);
                    }
                    pen += glyph.advance;
                }
                content.push_str("> Tj ET\n");
                x += run.iter().map(|g| g.advance).sum::<f64>();
                start = end;
            }
        }
    }
    Ok(Drawn {
        content,
        bounds: if x0 <= x1 && y0 <= y1 {
            [x0, y0, x1, y1]
        } else {
            [0.0, 0.0, 1.0, 1.0]
        },
    })
}

/// A PDF of a page for each of `pages`, its blocks drawn, each page's box
/// around what it draws. The fonts are embedded once, for all pages.
pub(crate) fn render(pages: &[&[TextBlock]]) -> Result<Vec<u8>, Refusal> {
    let mut maps: BTreeMap<ReportFace, BTreeMap<u16, char>> = BTreeMap::new();
    let drawn = pages
        .iter()
        .map(|blocks| draw(blocks, &mut maps))
        .collect::<Result<Vec<_>, _>>()?;
    let count = i32::try_from(drawn.len()).map_err(|_| Refusal::Overflow)?;
    // Catalog 1, page tree 2, then each page and its content, then the
    // fonts, ten numbers each.
    let page_ref = |i: i32| Ref::new(3 + 2 * i);
    let first_font = 3 + 2 * count;
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2))
        .kids((0..count).map(page_ref))
        .count(count);
    let bases: Vec<(ReportFace, i32)> = maps
        .keys()
        .enumerate()
        .map(|(i, face)| (*face, first_font + 10 * i as i32))
        .collect();
    for (i, drawn) in (0..count).zip(&drawn) {
        let [x0, y0, x1, y1] = drawn.bounds;
        let mut page = pdf.page(page_ref(i));
        page.parent(Ref::new(2))
            .media_box(Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32))
            .contents(Ref::new(4 + 2 * i));
        {
            let mut resources = page.resources();
            let mut fonts = resources.fonts();
            for (face, base) in &bases {
                fonts.pair(Name(face.names().0), Ref::new(*base));
            }
        }
        page.finish();
        pdf.stream(Ref::new(4 + 2 * i), drawn.content.as_bytes());
    }
    for (face, base) in &bases {
        embed(&mut pdf, *face, *base, &maps[face]);
    }
    Ok(pdf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::runtime::dynapdf_text::Span;

    /// The page's box holds every glyph's outline, not only the advances:
    /// the worker draws the page as a form clipped to it, so a `j`'s tail,
    /// left of its pen, must be inside. A word wider than its rectangle is
    /// refused rather than drawn past it.
    #[test]
    fn the_box_holds_the_glyph_outlines_and_wide_words_are_refused() {
        let at = |text: &str, size: f64, width: f64| TextBlock {
            left: 100.0,
            top: 200.0,
            width,
            height: None,
            align: Align::Left,
            spans: vec![Span {
                face: Face::Sans,
                size,
                color: [0.0; 3],
                text: text.into(),
            }],
            ink_inside: false,
        };
        let block = |text: &str, width: f64| at(text, 100.0, width);
        let pdf = render(&[&[block("j", 300.0)]]).unwrap();
        let parsed = hayro::hayro_interpret::hayro_syntax::Pdf::new(pdf).unwrap();
        let media = parsed.pages()[0].media_box();
        assert!(media.x0 < 99.0, "the tail of the j: {media:?}");
        assert_eq!(check(&block("WWWW", 50.0)), Err(Refusal::TooWide));
        // The spaces before a word count toward its line; after it, not.
        assert_eq!(check(&at("     a", 12.0, 20.0)), Err(Refusal::TooWide));
        assert_eq!(check(&at("a     ", 12.0, 20.0)), Ok(()));
        // A caption's ink must fit: a j whose advance fits 23 points but
        // whose tail does not.
        let caption = TextBlock {
            align: Align::Center,
            height: Some(150.0),
            ink_inside: true,
            ..block("j", 23.0)
        };
        assert_eq!(
            check(&TextBlock {
                ink_inside: false,
                ..caption.clone()
            }),
            Ok(())
        );
        assert_eq!(check(&caption), Err(Refusal::TooWide));
    }
}
