//! Portable vector reports. Pagination is shared with future Preview/printing;
//! this backend embeds the bundled OFL Inter and Liberation Sans fonts,
//! independently of OS fonts.
//! Static layout pictures are decoded once per owner and embedded as image
//! XObjects; nothing else is rasterized.
//! Unsupported content and text overflow fail before any destination is opened.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use om_layout::pagination::{paginate, BreakGroups, PageSize, ReportPlan};
use om_layout::{
    sliding, BreakField, Color, GraphicFormat, Layout, LayoutObject, LibraryImage,
    LibraryImageFormat, ObjectKind, Part, PartKind, Rect, ResolvedStyle,
};
use pdf_writer::types::{CidFontType, FontFlags, PageMode, SystemInfo, UnicodeCmap};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Ref, Str};

const REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
/// Liberation Sans 2.1.5 (SIL OFL 1.1), metric-compatible with Arial: reports
/// set layout text whose font is Arial or Helvetica in it, so lines wrap and
/// cut where FileMaker's do.
pub(crate) const SANS: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Regular.ttf");
pub(crate) const SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Bold.ttf");
/// Liberation Sans Italic and Bold Italic 2.1.5, metric-compatible with
/// Arial Italic and Arial Bold Italic.
pub(crate) const SANS_ITALIC: &[u8] = include_bytes!("../assets/fonts/LiberationSans-Italic.ttf");
pub(crate) const SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/LiberationSans-BoldItalic.ttf");

/// The faces report text is set in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ReportFace {
    Inter,
    InterBold,
    /// Liberation Sans, for Arial and Helvetica.
    Sans,
    SansBold,
    SansItalic,
    SansBoldItalic,
}

impl ReportFace {
    pub(crate) const ALL: [ReportFace; 6] = [
        ReportFace::Inter,
        ReportFace::InterBold,
        ReportFace::Sans,
        ReportFace::SansBold,
        ReportFace::SansItalic,
        ReportFace::SansBoldItalic,
    ];

    /// The face for text in `style`: Liberation Sans when the style's first
    /// font family is Arial or Helvetica, else Inter; bold from weight 2 or
    /// 600 and up; italic from font-style italic or oblique (2, 3). No
    /// italic Inter is bundled, so italic text in another family refuses
    /// rather than print upright.
    fn of(style: &ResolvedStyle) -> Result<Self, ReportError> {
        let bold = style.font_weight.is_some_and(|w| w == 2 || w >= 600);
        let italic = matches!(style.font_style, Some(2 | 3));
        let family = style
            .font_family
            .as_deref()
            .and_then(|families| families.split(',').next())
            .map(|family| {
                family
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_ascii_lowercase()
            });
        let sans = matches!(family.as_deref(), Some("arial" | "helvetica"));
        Ok(match (sans, bold, italic) {
            (false, false, false) => ReportFace::Inter,
            (false, true, false) => ReportFace::InterBold,
            (false, _, true) => {
                return Err(ReportError::unsupported(
                    "Italic report text is supported only in Arial and Helvetica",
                ))
            }
            (true, false, false) => ReportFace::Sans,
            (true, true, false) => ReportFace::SansBold,
            (true, false, true) => ReportFace::SansItalic,
            (true, true, true) => ReportFace::SansBoldItalic,
        })
    }

    fn index(self) -> usize {
        self as usize
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            ReportFace::Inter => REGULAR,
            ReportFace::InterBold => BOLD,
            ReportFace::Sans => SANS,
            ReportFace::SansBold => SANS_BOLD,
            ReportFace::SansItalic => SANS_ITALIC,
            ReportFace::SansBoldItalic => SANS_BOLD_ITALIC,
        }
    }

    pub(crate) fn face(self) -> ttf_parser::Face<'static> {
        ttf_parser::Face::parse(self.bytes(), 0).expect("bundled report font")
    }

    /// The page resource name, and the font's PostScript name.
    pub(crate) fn names(self) -> (&'static [u8], &'static [u8]) {
        match self {
            ReportFace::Inter => (b"F1", b"Inter-Regular"),
            ReportFace::InterBold => (b"F2", b"Inter-SemiBold"),
            ReportFace::Sans => (b"F3", b"LiberationSans"),
            ReportFace::SansBold => (b"F4", b"LiberationSans-Bold"),
            ReportFace::SansItalic => (b"F5", b"LiberationSans-Italic"),
            ReportFace::SansBoldItalic => (b"F6", b"LiberationSans-BoldItalic"),
        }
    }

    pub(crate) fn is_sans(self) -> bool {
        !matches!(self, ReportFace::Inter | ReportFace::InterBold)
    }
}

#[derive(Clone, Debug)]
pub struct ReportError {
    pub code: i32,
    pub message: String,
}
impl ReportError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
}

/// What a viewer shows beside the page when it opens the PDF: Save Records
/// as PDF's restored Initial View option (script-steps.md 11.7, its values
/// as Create PDF's `View` names them in 12.5), written as the catalog's
/// `/PageMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PdfView {
    /// 0 Page Only: no `/PageMode`, the PDF default (`/UseNone`).
    #[default]
    PageOnly,
    /// 1 Bookmarks Panel and Page: `/UseOutlines`.
    Bookmarks,
    /// 2 Pages Panel and Page: `/UseThumbs`.
    Pages,
}

/// The planned document and its complete, validated PDF. No file I/O is done.
#[derive(Debug)]
pub struct PdfReport {
    pub plan: ReportPlan,
    pub bytes: Vec<u8>,
}

/// A color as the PDF paints it: RGB, and its opacity in 255ths when it is
/// translucent (8 bits, as Browse's `Color32` keeps it).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Paint {
    rgb: [f32; 3],
    alpha: Option<u8>,
}

/// `color` as a [`Paint`]; `None` when it is fully transparent (nothing is
/// drawn).
fn paint(color: Color) -> Result<Option<Paint>, ReportError> {
    let values = [color.red, color.green, color.blue, color.alpha];
    if values
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(ReportError::unsupported(
            "Invalid report colors are unsupported",
        ));
    }
    let alpha = (color.alpha * 255.0).round() as u8;
    Ok((alpha != 0).then_some(Paint {
        rgb: [color.red, color.green, color.blue],
        alpha: (alpha != u8::MAX).then_some(alpha),
    }))
}

/// The page resource name of the graphics state with opacity `alpha`.
fn alpha_name(alpha: u8) -> String {
    format!("A{alpha}")
}

/// Before painting with `paint`: a translucent paint opens a graphics state
/// with its opacity (fill and stroke alike, as one paint operation follows),
/// recorded in `alphas` for the page's resources. [`close`] ends it after
/// that operation, so later drawing is opaque again. Returns whether a
/// state was opened. Call it before building the path.
fn open(c: &mut Content, paint: &Paint, alphas: &mut BTreeSet<u8>) -> bool {
    let Some(alpha) = paint.alpha else {
        return false;
    };
    alphas.insert(alpha);
    c.save_state()
        .set_parameters(Name(alpha_name(alpha).as_bytes()));
    true
}
fn close(c: &mut Content, opened: bool) {
    if opened {
        c.restore_state();
    }
}

/// Opens `color` ([`open`]) and sets it as the fill color: `Some(opened)`
/// when it paints anything.
fn fill(
    c: &mut Content,
    color: Option<Color>,
    alphas: &mut BTreeSet<u8>,
) -> Result<Option<bool>, ReportError> {
    let Some(paint) = color.map(paint).transpose()?.flatten() else {
        return Ok(None);
    };
    let opened = open(c, &paint, alphas);
    c.set_fill_rgb(paint.rgb[0], paint.rgb[1], paint.rgb[2]);
    Ok(Some(opened))
}
fn pdf_rect(c: &mut Content, r: &Rect, height: f64) {
    c.rect(
        r.left as f32,
        (height - r.bottom) as f32,
        r.width() as f32,
        r.height() as f32,
    );
}
/// Style properties 32-35: the top-right, bottom-right, bottom-left and
/// top-left corner radii (layout-model.md 6.5).
const CORNER_RADII: [i32; 4] = [32, 33, 34, 35];

/// The radius `what`'s four corners share, in points (0 for square
/// corners), read through `property` ([`om_layout::Property::radius_points`]:
/// a length or a `coordVal` of two equal lengths). Corners that differ, an
/// elliptical corner, and a radius in a relative unit (`%`, `em`, `line`)
/// or an automatic value refuse the report: drawing such a frame square,
/// or with guessed corners, would not be what FileMaker shows.
fn corner_radius<'a>(
    property: impl Fn(i32) -> Option<&'a om_layout::Property>,
    what: &str,
) -> Result<f64, ReportError> {
    let mut shared = None;
    for id in CORNER_RADII {
        let corner = match property(id) {
            None => 0.0,
            Some(p) => p
                .radius_points()
                .filter(|radius| *radius >= 0.0)
                .ok_or_else(|| {
                    ReportError::unsupported(format!(
                        "{what} has an elliptical corner or a corner radius in a relative unit \
                         or an automatic value; only circular absolute radii print"
                    ))
                })?,
        };
        match shared {
            None => shared = Some(corner),
            Some(radius) if radius == corner => {}
            Some(_) => {
                return Err(ReportError::unsupported(format!(
                    "{what} has corners of different radii; only equal corners print"
                )))
            }
        }
    }
    Ok(shared.unwrap_or(0.0))
}

/// `radius` reduced to half of `r`'s smaller side, as CSS reduces it.
fn fitted_radius(r: &Rect, radius: f64) -> f64 {
    radius.min(r.width() / 2.0).min(r.height() / 2.0).max(0.0)
}

/// The closed outline of `r` with corners of `radius` (already fitted):
/// four Bézier quarter arcs, as ovals are drawn.
fn rounded_rect(c: &mut Content, r: &Rect, height: f64, radius: f64) {
    let (left, right) = (r.left as f32, r.right as f32);
    let (top, bottom) = ((height - r.top) as f32, (height - r.bottom) as f32);
    let radius = radius as f32;
    let k = 0.552_284_8 * radius;
    c.move_to(left + radius, top)
        .line_to(right - radius, top)
        .cubic_to(
            right - radius + k,
            top,
            right,
            top - radius + k,
            right,
            top - radius,
        )
        .line_to(right, bottom + radius)
        .cubic_to(
            right,
            bottom + radius - k,
            right - radius + k,
            bottom,
            right - radius,
            bottom,
        )
        .line_to(left + radius, bottom)
        .cubic_to(
            left + radius - k,
            bottom,
            left,
            bottom + radius - k,
            left,
            bottom + radius,
        )
        .line_to(left, top - radius)
        .cubic_to(
            left,
            top - radius + k,
            left + radius - k,
            top,
            left + radius,
            top,
        )
        .close_path();
}

/// An object's or part's fill and borders. `radius` is the corners'
/// (`corner_radius`): a rounded frame's fill takes the rounded shape, and
/// its border, which must be the same on every side, follows it inside the
/// bounds.
fn box_style(
    c: &mut Content,
    r: &Rect,
    height: f64,
    style: &ResolvedStyle,
    radius: f64,
    alphas: &mut BTreeSet<u8>,
) -> Result<(), ReportError> {
    let radius = fitted_radius(r, radius);
    if let Some(opened) = fill(c, style.background, alphas)? {
        if radius > 0.0 {
            rounded_rect(c, r, height, radius);
        } else {
            pdf_rect(c, r, height);
        }
        c.fill_nonzero();
        close(c, opened);
    }
    if radius > 0.0 {
        return rounded_border(c, r, height, style, radius, alphas);
    }
    for side in 0..4 {
        let width = style.border_width[side].unwrap_or(0.0);
        if !width.is_finite() || width < 0.0 {
            return Err(ReportError::unsupported("Invalid border width"));
        }
        if width == 0.0 || matches!(style.border_style[side], Some(0 | 1)) {
            continue;
        }
        if !matches!(style.border_style[side], None | Some(5)) {
            return Err(ReportError::unsupported(
                "Only solid report borders are supported",
            ));
        }
        let color = style.border_color[side].unwrap_or(Color {
            alpha: 1.0,
            ..Default::default()
        });
        let Some(paint) = paint(color)? else {
            continue;
        };
        let opened = open(c, &paint, alphas);
        c.set_stroke_rgb(paint.rgb[0], paint.rgb[1], paint.rgb[2])
            .set_line_width(width as f32);
        // Keep the complete stroke inside the object's bounds.
        let (l, t, rr, b) = (
            r.left + width / 2.0,
            r.top + width / 2.0,
            r.right - width / 2.0,
            r.bottom - width / 2.0,
        );
        let (x, y, xx, yy) = match side {
            0 => (l, t, rr, t),
            1 => (rr, t, rr, b),
            2 => (rr, b, l, b),
            _ => (l, b, l, t),
        };
        c.move_to(x as f32, (height - y) as f32)
            .line_to(xx as f32, (height - yy) as f32)
            .stroke();
        close(c, opened);
    }
    Ok(())
}

/// A rounded frame's border: one stroke along the rounded outline, inset by
/// half its width so it stays inside the bounds, as the square frame's
/// sides do. Every side must draw the same solid border, or none may.
fn rounded_border(
    c: &mut Content,
    r: &Rect,
    height: f64,
    style: &ResolvedStyle,
    radius: f64,
    alphas: &mut BTreeSet<u8>,
) -> Result<(), ReportError> {
    let mut sides = Vec::with_capacity(4);
    for side in 0..4 {
        let width = style.border_width[side].unwrap_or(0.0);
        if !width.is_finite() || width < 0.0 {
            return Err(ReportError::unsupported("Invalid border width"));
        }
        if width == 0.0 || matches!(style.border_style[side], Some(0 | 1)) {
            sides.push(None);
            continue;
        }
        if !matches!(style.border_style[side], None | Some(5)) {
            return Err(ReportError::unsupported(
                "Only solid report borders are supported",
            ));
        }
        let color = paint(style.border_color[side].unwrap_or(Color {
            alpha: 1.0,
            ..Default::default()
        }))?;
        sides.push(color.map(|color| (width, color)));
    }
    if sides.iter().any(|side| *side != sides[0]) {
        return Err(ReportError::unsupported(
            "A rounded report frame's border differs between its sides; only the same border on \
             every side prints",
        ));
    }
    let Some((width, paint)) = sides[0] else {
        return Ok(());
    };
    if width >= r.width().min(r.height()) {
        return Err(ReportError::unsupported(
            "A rounded report frame's border is too wide for its bounds",
        ));
    }
    let inset = Rect {
        left: r.left + width / 2.0,
        top: r.top + width / 2.0,
        right: r.right - width / 2.0,
        bottom: r.bottom - width / 2.0,
        ..Default::default()
    };
    let opened = open(c, &paint, alphas);
    c.set_stroke_rgb(paint.rgb[0], paint.rgb[1], paint.rgb[2])
        .set_line_width(width as f32);
    rounded_rect(
        c,
        &inset,
        height,
        fitted_radius(&inset, radius - width / 2.0),
    );
    c.stroke();
    close(c, opened);
    Ok(())
}

/// `object`'s text style with the conditional formats `conditions` laid
/// over it as Browse lays them: over the theme and local style, and then
/// over a stored character style, whose font, size, color and faces a
/// condition replaces.
fn text_style(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
) -> Result<ResolvedStyle, ReportError> {
    let mut style = crate::document::text_style(layout, object, conditions, 1);
    if let Some(rich) = &object.rich_text {
        if let Some(first) = rich.character_runs.first() {
            if rich.character_runs.iter().any(|r| r.style != first.style) {
                return Err(ReportError::unsupported(
                    "Mixed character styles are unsupported",
                ));
            }
            let ch = &first.style;
            // The run's own font, as Browse sets it (`rich_text::text_format`):
            // its face, italic included, is chosen by it.
            style.font_family = ch.font_family.clone().or(style.font_family);
            style.font_size = ch.font_size.or(style.font_size);
            style.color = ch.color.or(style.color);
            if let Some(faces) = ch.faces {
                if faces & !(256 | 512) != 0 {
                    return Err(ReportError::unsupported(
                        "Only regular, bold and italic report text is supported",
                    ));
                }
                if ch.face_mask.unwrap_or(u16::MAX) & 256 != 0 {
                    style.font_weight = Some(if faces & 256 != 0 { 700 } else { 400 });
                }
                // Italic as font-style italic (2), upright as normal (1).
                if ch.face_mask.unwrap_or(u16::MAX) & 512 != 0 {
                    style.font_style = Some(if faces & 512 != 0 { 2 } else { 1 });
                }
            }
        }
        if let Some(first) = rich.paragraph_runs.first() {
            if rich.paragraph_runs.iter().any(|r| r.style != first.style) {
                return Err(ReportError::unsupported(
                    "Mixed paragraph styles are unsupported",
                ));
            }
            style.text_align = first.style.text_align.or(style.text_align);
            if let Some(height) = first.style.line_height {
                if style.line_height.is_none() {
                    let (value, unit) = match first.style.line_height_unit.unwrap_or(0) {
                        0 => (f64::from(height), 9),
                        1 => (f64::from(height), 6),
                        2 => (f64::from(height) * 72.0 / 2.54, 6),
                        3 => (f64::from(height) * 72.0, 6),
                        _ => {
                            return Err(ReportError::unsupported(
                                "Unsupported rich paragraph leading units",
                            ))
                        }
                    };
                    style.line_height = Some(om_layout::Length {
                        value,
                        unit,
                        ..Default::default()
                    });
                }
            }
        }
    }
    if !conditions.is_empty() && object.rich_text.is_some() {
        let condition = crate::document::conditional_text_style(layout, object, conditions, 1);
        style.font_family = condition.font_family.or(style.font_family);
        style.font_size = condition.font_size.or(style.font_size);
        style.color = condition.color.or(style.color);
        style.font_weight = condition.font_weight.or(style.font_weight);
        style.font_style = condition.font_style.or(style.font_style);
    }
    Ok(style)
}

/// The style text is laid out in: [`text_style`], with the text
/// component's margins (properties 105-108, top, right, bottom, left;
/// layout-model.md 6.5) added to its padding. The starter lists' edit boxes
/// place their text with these: 0.875em above, 0.625em below and 0.25em at
/// the sides. A container's picture and file keep the plain padding.
fn text_box_style(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
) -> Result<ResolvedStyle, ReportError> {
    let mut style = text_style(layout, object, conditions)?;
    let size = style.font_size.unwrap_or(12.0);
    // An automatic keyword, a percent or a line count is no concrete inset.
    let Some(margins) = crate::document::text_margins(layout, object, size) else {
        return Err(ReportError::unsupported(format!(
            "Report object {} has a text margin in a relative unit or an automatic value",
            object.id
        )));
    };
    for (padding, margin) in style.padding.iter_mut().zip(margins) {
        *padding = Some(padding.unwrap_or(0.0) + margin);
    }
    // A field's text box is also inset by its border widths. FileMaker Pro
    // 26 clips a starter edit box's, drop-down list's or calendar's text to
    // its frame less 1 point all round and then its margins, though only its
    // bottom border has a style; a pop-up menu's only on the sides it draws
    // a border on (layout-model.md 6.5).
    if object.kind() == ObjectKind::Field {
        let field = object.field_obj.as_ref();
        let pop_up = field.is_some_and(|field| field.display_type == 2);
        for (i, width) in style.border_width.into_iter().enumerate() {
            let width = width.unwrap_or(0.0);
            let drawn = !matches!(style.border_style[i], Some(0 | 1));
            if width.is_finite() && width > 0.0 && (drawn || !pop_up) {
                style.padding[i] = Some(style.padding[i].unwrap_or(0.0) + width.min(1000.0));
            }
        }
        // A field that shows its arrow or calendar icon keeps the icon
        // component's width clear at the right, though a report prints no
        // icon: 30 points on the starters.
        if field.is_some_and(|field| field.has_icon()) {
            if let Some(length) = layout
                .object_property(object, 4, 1, 109)
                .and_then(|p| p.length.as_ref())
            {
                let width = match (length.unit, length.auto_value) {
                    (7, 0 | 1) => Some(length.value * size),
                    _ => length.points(),
                };
                let Some(width) = width.filter(|w| w.is_finite() && *w >= 0.0) else {
                    return Err(ReportError::unsupported(format!(
                        "Report field {} has an icon width in a relative unit or an automatic value",
                        object.id
                    )));
                };
                style.padding[1] = Some(style.padding[1].unwrap_or(0.0) + width.min(1000.0));
            }
        }
    }
    for (pad, inset) in style.padding.iter_mut().zip(label_inset(object)) {
        if inset > 0.0 {
            *pad = Some(pad.unwrap_or(0.0) + inset);
        }
    }
    Ok(style)
}

/// The inset a text object's or button's text has inside its frame, with no
/// border or margin stored for it (top, right, bottom, left). FileMaker Pro
/// 26 clips a starter label's text to its frame less 1 point on every side,
/// and a button's label to its frame less 2 points above and 1 at the left
/// (layout-model.md 6.5).
fn label_inset(object: &LayoutObject) -> [f64; 4] {
    match object.kind() {
        ObjectKind::Text => [1.0; 4],
        ObjectKind::Button => [2.0, 0.0, 0.0, 1.0],
        _ => [0.0; 4],
    }
}

/// Wrapped lines of `value` in `r`, and the height of `r` its lines need
/// (padding included; zero for no text): a shrinking field's printed height.
/// Text that does not fit `r` refuses the report.
fn prepare_text(
    value: &str,
    r: &Rect,
    style: &ResolvedStyle,
) -> Result<(Vec<TextLine>, f64), ReportError> {
    prepare_text_in(value, r, style, Overflow::Refuse).map(|(lines, needed, _)| (lines, needed))
}

/// What [`prepare_text_in`] does with text taller than its box.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    Refuse,
    /// Lay the text out from the top of the box and keep the lines that
    /// start inside it, for the caller to clip to the box: a field prints
    /// only what fits within its bounds, as FileMaker prints it. A glyph
    /// wider than the box takes a line of its own and is cut too.
    Clip,
}

/// What `object`'s text does when it does not fit. A field prints only what
/// fits within its bounds, as FileMaker prints it (Claris: sliding exists to
/// print more). A text object or button label is cut at its text box too:
/// Pro clips every label's text to it (layout-model.md 6.5). A field that
/// slides (`slides`, [`PartObjects::slides`]) keeps its measured height and
/// refuses as before.
fn overflow_of(object: &LayoutObject, slides: bool) -> Overflow {
    match object.kind() {
        ObjectKind::Field if !slides => Overflow::Clip,
        ObjectKind::Text | ObjectKind::Button => Overflow::Clip,
        _ => Overflow::Refuse,
    }
}

/// [`prepare_text`], and whether the text overflowed `r` (only with
/// [`Overflow::Clip`]).
fn prepare_text_in(
    value: &str,
    r: &Rect,
    style: &ResolvedStyle,
    overflow: Overflow,
) -> Result<(Vec<TextLine>, f64, bool), ReportError> {
    if value.is_empty() {
        return Ok((Vec::new(), 0.0, false));
    }
    let size = style.font_size.unwrap_or(12.0);
    if !size.is_finite() || !(1.0..=512.0).contains(&size) {
        return Err(ReportError::unsupported("Invalid report font size"));
    }
    let report_face = ReportFace::of(style)?;
    let face = report_face.face();
    let unit = size / f64::from(face.units_per_em());
    let ascent = f64::from(face.ascender()) * unit;
    let descent = -f64::from(face.descender()) * unit;
    // Pro 26 sets a first baseline a whole number of points below the top
    // of its text box: the ascent, rounded (12 for 13-point Arial, 14 for
    // 16-point; layout-model.md 6.5).
    let first_baseline = ascent.round();
    let pitch = match &style.line_height {
        None => (ascent + descent).max(size * 1.2),
        Some(l) if l.unit == 9 => (ascent + descent) * l.value,
        Some(l) if matches!(l.unit, 0 | 6 | 8) => l.value,
        _ => {
            return Err(ReportError::unsupported(
                "Unsupported report line height units",
            ))
        }
    };
    if !pitch.is_finite() || pitch < ascent + descent {
        return Err(ReportError::unsupported(
            "Report leading would overlap lines",
        ));
    }
    let pad = style.padding.map(|p| p.unwrap_or(0.0));
    if pad.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return Err(ReportError::unsupported("Invalid text padding"));
    }
    let width = r.width() - pad[1] - pad[3];
    let available = r.height() - pad[0] - pad[2];
    let transformed = match style.text_transform {
        None | Some(0 | 1) => value.to_owned(),
        Some(3) => value.to_uppercase(),
        Some(4) => value.to_lowercase(),
        _ => {
            return Err(ReportError::unsupported(
                "Unsupported report text transform",
            ))
        }
    };
    // Normalize FileMaker paragraphs. Prefer whitespace breaks, falling back
    // to character breaks for long tokens. Preserve all characters.
    let normalized = transformed.replace("\r\n", "\n").replace('\r', "\n");
    type MeasuredLine = (Vec<(u16, f64, char)>, f64);
    let mut lines: Vec<MeasuredLine> = Vec::new();
    for paragraph in normalized.split('\n') {
        let mut glyphs = Vec::new();
        for ch in paragraph.chars() {
            // These ranges need shaping/bidi or combining mark positioning.
            if ch.is_control()
                || matches!(ch as u32, 0x300..=0x36f | 0x590..=0x1cff | 0x200b..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069 | 0xfb1d..=0xfdff | 0xfe70..=0xfeff)
            {
                return Err(ReportError::unsupported(format!(
                    "Report text requires unsupported shaping or control character U+{:04X}",
                    ch as u32
                )));
            }
            let glyph = face.glyph_index(ch).ok_or_else(|| {
                ReportError::unsupported(format!(
                    "The report font has no glyph for U+{:04X}",
                    ch as u32
                ))
            })?;
            let advance = f64::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * unit;
            if advance > width && (overflow == Overflow::Refuse || width <= 0.0) {
                return Err(ReportError::unsupported(
                    "A report glyph is wider than its text box",
                ));
            }
            glyphs.push((glyph.0, advance, ch));
        }
        if glyphs.is_empty() {
            lines.push((Vec::new(), 0.0));
        }
        let mut start = 0;
        while start < glyphs.len() {
            let (mut end, mut used, mut word_end) = (start, 0.0, None);
            while end < glyphs.len() && used + glyphs[end].1 <= width + 0.001 {
                used += glyphs[end].1;
                end += 1;
                if glyphs[end - 1].2.is_whitespace() {
                    word_end = Some(end);
                }
            }
            if end < glyphs.len() {
                end = word_end.unwrap_or(end);
            }
            // A glyph wider than the box (only when clipping) is a line.
            if end == start {
                end = start + 1;
            }
            let line = &glyphs[start..end];
            lines.push((line.to_vec(), line.iter().map(|g| g.1).sum()));
            start = end;
        }
    }
    let required = ascent + descent + (lines.len() - 1) as f64 * pitch;
    let overflowed = required > available + 0.01;
    if overflowed && overflow == Overflow::Refuse {
        return Err(ReportError::unsupported(
            "Report text overflows its object; enlarge the box or reduce the font size",
        ));
    }
    let dy = match style.vertical_align {
        // Overflowing text starts at the top of its box.
        _ if overflowed => 0.0,
        None | Some(0) => 0.0,
        Some(1) => (available - required) / 2.0,
        Some(2) => available - required,
        _ => {
            return Err(ReportError::unsupported(
                "Unsupported vertical text alignment",
            ))
        }
    };
    let needed = pad[0] + required + pad[2];
    // Clipped text is also cut where its rounded baseline carries the last
    // line's descent past the box, or a glyph wider than the box past its
    // sides, though the exact metrics fit.
    let cut = overflowed
        || overflow == Overflow::Clip
            && (dy + first_baseline + descent + (lines.len() - 1) as f64 * pitch
                > available + 0.01
                || lines.iter().any(|(_, used)| *used > width + 0.001));
    let color = style.color.unwrap_or(Color {
        alpha: 1.0,
        ..Default::default()
    });
    if paint(color)?.is_none() {
        return Ok((Vec::new(), needed, overflowed));
    }
    let mut output = Vec::new();
    for (i, (glyphs, used)) in lines.into_iter().enumerate() {
        // A line that starts below the box shows nothing of itself.
        let top = r.top + pad[0] + i as f64 * pitch;
        if overflowed && top >= r.bottom {
            break;
        }
        let dx = match style.text_align {
            None | Some(0 | 1) => 0.0,
            Some(2) => (width - used) / 2.0,
            Some(3) => width - used,
            _ => {
                return Err(ReportError::unsupported(
                    "Justified report text is unsupported",
                ))
            }
        };
        // A glyph whose outline reaches past the text box's sides (an
        // italic overhang, say), though its advance fits, is cut there, as
        // Pro clips text to its box.
        let (left, right) = (r.left + pad[3], (r.right - pad[1]).max(r.left + pad[3]));
        let mut pen = left + dx;
        let mut spills = false;
        for (glyph, advance, _) in &glyphs {
            if let Some(ink) = face.glyph_bounding_box(ttf_parser::GlyphId(*glyph)) {
                spills |= pen + f64::from(ink.x_min) * unit < left - 0.01
                    || pen + f64::from(ink.x_max) * unit > right + 0.01;
            }
            pen += advance;
        }
        output.push(TextLine {
            glyphs,
            x: r.left + pad[3] + dx,
            baseline: r.top + pad[0] + dy + first_baseline + i as f64 * pitch,
            size,
            face: report_face,
            color,
            // Cut at the text box: the field's frame less the padding and
            // text margins above and below. FileMaker Pro 26 keeps a wrapped
            // second line in its PDF's text but shows none of it (#536).
            // Ink that spills past the box's sides is cut at its sides
            // (a label's frame less its 1-point inset, layout-model.md 6.5),
            // and only there: a line that fits keeps the frame's height, so
            // an accent above the box is not cut for a bearing at its side.
            clip: (cut || spills).then(|| Rect {
                top: if cut { r.top + pad[0] } else { r.top },
                bottom: if cut {
                    (r.bottom - pad[2]).max(r.top + pad[0])
                } else {
                    r.bottom
                },
                left: if spills { left } else { r.left },
                right: if spills { right } else { r.right },
                ..r.clone()
            }),
        });
    }
    Ok((output, needed, overflowed))
}

/// Embeds `face`, whose glyphs `map` names, as six objects from `base`.
pub(crate) fn embed(pdf: &mut Pdf, face_id: ReportFace, base: i32, map: &BTreeMap<u16, char>) {
    let id = |offset| Ref::new(base + offset);
    let name = Name(face_id.names().1);
    let info = SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    };
    pdf.type0_font(id(0))
        .base_font(name)
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(id(1))
        .to_unicode(id(4));
    let face = face_id.face();
    let scale = 1000.0 / f32::from(face.units_per_em());
    let mut cid = pdf.cid_font(id(1));
    cid.subtype(CidFontType::Type2)
        .base_font(name)
        .system_info(info)
        .font_descriptor(id(2))
        .cid_to_gid_map_predefined(Name(b"Identity"));
    cid.widths().consecutive(
        0,
        (0..face.number_of_glyphs()).map(|g| {
            f32::from(face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0)) * scale
        }),
    );
    cid.finish();
    let bbox = face.global_bounding_box();
    let mut flags = FontFlags::NON_SYMBOLIC;
    if face.is_italic() {
        flags |= FontFlags::ITALIC;
    }
    pdf.font_descriptor(id(2))
        .name(name)
        .flags(flags)
        .bbox(pdf_writer::Rect::new(
            f32::from(bbox.x_min) * scale,
            f32::from(bbox.y_min) * scale,
            f32::from(bbox.x_max) * scale,
            f32::from(bbox.y_max) * scale,
        ))
        .italic_angle(face.italic_angle())
        .ascent(f32::from(face.ascender()) * scale)
        .descent(f32::from(face.descender()) * scale)
        .cap_height(f32::from(face.capital_height().unwrap_or(face.ascender())) * scale)
        .stem_v(80.0)
        .font_file2(id(3));
    pdf.stream(id(3), face_id.bytes())
        .pair(Name(b"Length1"), face_id.bytes().len() as i32);
    let mut cmap = UnicodeCmap::new(Name(b"Unicode"), info);
    for (&glyph, &ch) in map {
        cmap.pair(glyph, ch);
    }
    pdf.stream(id(4), &cmap.finish());
}

/// Measured with the advances of the bundled face the PDF embeds.
#[derive(Clone, Debug)]
pub(crate) struct TextLine {
    pub glyphs: Vec<(u16, f64, char)>,
    pub x: f64,
    pub baseline: f64,
    pub size: f64,
    pub face: ReportFace,
    pub color: Color,
    /// The box overflowing text is cut at ([`Overflow::Clip`]): its field's
    /// text box, between the padding and margins above and below, across
    /// the field's width; `None` when the text fits.
    pub clip: Option<Rect>,
}
#[derive(Clone, Debug)]
pub(crate) struct ReportObject {
    pub kind: ObjectKind,
    pub bounds: Rect,
    pub style: ResolvedStyle,
    pub text: Vec<TextLine>,
    /// A static Graphic's decoded picture. Like Browse (egui's
    /// `Image::paint_at`), both backends stretch it over the whole `bounds`,
    /// above the object's own fill and borders. A container field's picture
    /// goes where `placed` says instead.
    pub picture: Option<Arc<Picture>>,
    /// A container field's picture: the content box it is clipped to and
    /// where its graphic format places it (data-format.md).
    pub placed: Option<[Rect; 2]>,
    /// A container field's file: where its icon goes, its name in `text`.
    pub file_icon: Option<Rect>,
    /// The radius the frame's corners share, in points (0 for square), as
    /// `corner_radius` read it; both backends fit it to `bounds`.
    pub radius: f64,
    /// A button's icon and the square it fills, drawn over the frame and
    /// under the label, stretched like a picture.
    pub icon: Option<(Arc<Picture>, Rect)>,
    /// A line's start and end points in layout points ([`line_endpoints`]);
    /// `None` for other objects.
    pub line: Option<[(f64, f64); 2]>,
}

/// A line's endpoints within `r`: it starts at the left edge with
/// `kLineStartLeft` (`0x1000`), else the right, and at the top with
/// `kLineStartTop` (`0x2000`), else the bottom, and ends at the opposite
/// corner (layout-model.md 5.3.1). Without either bit, or with both, a
/// sloped line runs from top left to bottom right; with one, from bottom
/// left to top right.
fn line_endpoints(r: &Rect, flags: u64) -> [(f64, f64); 2] {
    let (left, top) = (flags & 0x1000 != 0, flags & 0x2000 != 0);
    let x = |start| if start { r.left } else { r.right };
    let y = |start| if start { r.top } else { r.bottom };
    [(x(left), y(top)), (x(!left), y(!top))]
}

/// The widest/tallest decoded report picture, in pixels. It keeps a picture
/// within every Preview texture and bounds the samples a PDF embeds.
const MAX_PICTURE_SIDE: u32 = 8192;

/// A decoded library image, shared by Preview and PDF/Print.
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    /// Oriented RGBA samples, row 0 at the top, with straight (not
    /// premultiplied) alpha: decoded exactly as Browse decodes them.
    pub rgba: Vec<u8>,
    pub opaque: bool,
    /// Preview's upload of these samples, released with the picture.
    pub texture: OnceLock<egui::TextureHandle>,
}
impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("opaque", &self.opaque)
            .finish_non_exhaustive()
    }
}

mod static_picture;
pub mod svg_worker;

/// An SVG drawn at `size` pixels as Browse draws it (what the worker does
/// not draw left out): in the SVG worker, or in this process in unit
/// tests (`static_picture::svg`).
pub(crate) fn browse_svg(data: &[u8], size: [u32; 2]) -> Result<Picture, String> {
    static_picture::svg(data, Some(size), svg_worker::Unsupported::Skip)
}

/// Static layout pictures have their own bounded decoder; SVG pictures are
/// never tinted or passed through the button-icon renderer.
fn decode_picture(key: u64, image: &LibraryImage) -> Result<Picture, ReportError> {
    static_picture::decode(key, image, None)
}

/// Container data's picture for a frame of `size` points, bounded as Browse
/// bounds it (16,384 pixels a side, 256 MiB per decode), turned by its EXIF
/// orientation and kept at most at 300 pixels an inch of the size it prints
/// at (and [`MAX_PICTURE_SIDE`]); `None` when it does not decode, which
/// prints the field empty as Browse shows it.
fn decode_container_picture(
    data: &[u8],
    format: GraphicFormat,
    size: (f64, f64),
) -> ContainerPicture {
    use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
    let decoded = std::panic::catch_unwind(|| {
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
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(256 << 20);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().ok()?;
        let mut picture = DynamicImage::from_decoder(decoder).ok()?;
        picture.apply_orientation(orientation);
        Some(picture)
    })
    .ok()
    .flatten()?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return None;
    }
    // A picture pixel is a point, as in Browse.
    let placed = format.place((f64::from(width), f64::from(height)), size);
    let wanted =
        |points: f64, pixels: u32| (points * 300.0 / 72.0).ceil().clamp(1.0, f64::from(pixels));
    let scale = (wanted(placed[2], width) / f64::from(width))
        .min(wanted(placed[3], height) / f64::from(height))
        .min(f64::from(MAX_PICTURE_SIDE) / f64::from(width.max(height)));
    let picture = if scale < 1.0 {
        let side = |pixels: u32| ((f64::from(pixels) * scale).round() as u32).max(1);
        decoded.resize_exact(
            side(width),
            side(height),
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        decoded
    };
    let rgba = picture.into_rgba8();
    let (width, height) = rgba.dimensions();
    let rgba = rgba.into_raw();
    Some((
        Arc::new(Picture {
            width,
            height,
            opaque: rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == u8::MAX),
            rgba,
            texture: OnceLock::new(),
        }),
        placed,
    ))
}

/// Pictures decoded for one owner (a Preview session's layout, or one report
/// document), by library key. An entry is reused only for the identical
/// encoded image, so a key reused by another file, layout or snapshot decodes
/// its own bytes instead of showing stale pixels.
#[derive(Default)]
pub(crate) struct Pictures(
    RefCell<BTreeMap<PictureKey, DecodedPicture>>,
    RefCell<BTreeMap<ContainerKey, ContainerPicture>>,
    RefCell<BTreeMap<IconKey, DecodedPicture>>,
    /// Where SVG pictures and icons are drawn apart
    /// ([`Pictures::drawn_apart`]).
    Option<Drawing>,
);

/// What is drawn apart: a static picture or a button icon, by its cache
/// key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ApartKey {
    Picture(PictureKey),
    Icon(IconKey),
}

/// A drawing waiting for the drawing thread.
type ApartJob = Box<dyn FnOnce(&LibraryImage) -> Result<Arc<Picture>, ReportError> + Send>;

/// How many drawings wait at most, all Preview windows together: a request
/// past them drops the oldest, whose window is told so that it asks again
/// if it still shows it.
const APART_QUEUED: usize = 8;
/// How many drawn pictures one session keeps waiting to be taken; the
/// oldest goes first.
const APART_DRAWN: usize = 8;

/// One Preview session's SVG pictures and icons drawn apart, on the
/// process's one drawing thread ([`DRAWER`]): it draws one picture at a
/// time, oldest request first, runs only while some wait, and draws
/// nothing for a session that is gone or was cleared since it asked.
#[derive(Default)]
pub(crate) struct Drawing(Arc<Mutex<Owner>>);

#[derive(Default)]
struct Owner {
    /// Drawn and not taken yet, oldest first, at most [`APART_DRAWN`].
    drawn: std::collections::VecDeque<(ApartKey, DecodedPicture)>,
    /// Asked for in this generation and not drawn yet (waiting or being
    /// drawn): asked again, they are not drawn twice.
    pending: BTreeSet<ApartKey>,
    /// Bumped when the pictures are cleared: what was asked before is not
    /// kept.
    generation: u64,
    /// Called after each drawing of this session ends, kept or not, for
    /// Preview to draw its page again.
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Which session (while it lasts), picture and generation a drawing is
/// for.
struct Ticket {
    owner: std::sync::Weak<Mutex<Owner>>,
    key: ApartKey,
    generation: u64,
}

/// A drawing asked for.
struct DrawJob {
    ticket: Ticket,
    image: LibraryImage,
    draw: ApartJob,
}

/// The drawings waiting for the drawing thread, and whether it runs.
struct Drawer {
    queue: std::collections::VecDeque<DrawJob>,
    running: bool,
}

static DRAWER: Mutex<Drawer> = Mutex::new(Drawer {
    queue: std::collections::VecDeque::new(),
    running: false,
});

impl Ticket {
    /// Ends the drawing: keeps what it drew, if anything, for a session
    /// that still waits for it, and with `tell` tells the session, which
    /// asks again on its next repaint for what it still needs.
    fn end(self, drawn: Option<DecodedPicture>, tell: bool) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let notify = {
            let mut owner = lock(&owner);
            if owner.generation == self.generation {
                owner.pending.remove(&self.key);
                if let Some(drawn) = drawn {
                    if owner.drawn.len() >= APART_DRAWN {
                        owner.drawn.pop_front();
                    }
                    owner.drawn.push_back((self.key, drawn));
                }
            }
            owner.notify.clone().filter(|_| tell)
        };
        if let Some(notify) = notify {
            notify();
        }
    }

    /// Whether its session is still there and asks in the same
    /// generation.
    fn live(&self) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| lock(&owner).generation == self.generation)
    }
}

/// The drawing thread: draws what waits, oldest first, skipping what no
/// session waits for any more, and ends when nothing waits. A drawing that
/// panics is an error, so its picture never stays pending.
fn run_drawer() {
    loop {
        let DrawJob {
            ticket,
            image,
            draw,
        } = {
            let mut drawer = lock(&DRAWER);
            match drawer.queue.pop_front() {
                Some(job) => job,
                None => {
                    drawer.running = false;
                    return;
                }
            }
        };
        if !ticket.live() {
            ticket.end(None, true);
            continue;
        }
        let picture = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(&image)))
            .unwrap_or_else(|_| {
                Err(ReportError::unsupported(
                    "A report picture could not be drawn",
                ))
            });
        ticket.end(Some((image, picture)), true);
    }
}

impl Drawing {
    /// The picture `draw` makes of `image` for `key`, once drawn (taken out
    /// of here for the caller's cache); pending until then. A drawing of
    /// bytes the layout no longer has is drawn again.
    fn take_or_start(
        &self,
        key: ApartKey,
        image: &LibraryImage,
        draw: impl FnOnce(&LibraryImage) -> Result<Arc<Picture>, ReportError> + Send + 'static,
    ) -> Result<Arc<Picture>, ReportError> {
        let generation = {
            let mut owner = lock(&self.0);
            if let Some(at) = owner.drawn.iter().position(|(k, _)| *k == key) {
                if let Some((_, (source, picture))) = owner.drawn.remove(at) {
                    if source == *image {
                        return picture;
                    }
                }
            }
            if !owner.pending.insert(key) {
                return Err(drawing_pictures());
            }
            owner.generation
        };
        let job = DrawJob {
            ticket: Ticket {
                owner: Arc::downgrade(&self.0),
                key,
                generation,
            },
            image: image.clone(),
            draw: Box::new(draw),
        };
        let (dropped, start) = {
            let mut drawer = lock(&DRAWER);
            let dropped = (drawer.queue.len() >= APART_QUEUED)
                .then(|| drawer.queue.pop_front())
                .flatten();
            drawer.queue.push_back(job);
            let start = !drawer.running;
            drawer.running = true;
            (dropped, start)
        };
        if let Some(dropped) = dropped {
            dropped.ticket.end(None, true);
        }
        if start {
            if let Err(error) = std::thread::Builder::new()
                .name("om-preview-pictures".into())
                .spawn(run_drawer)
            {
                let waiting: Vec<DrawJob> = {
                    let mut drawer = lock(&DRAWER);
                    drawer.running = false;
                    drawer.queue.drain(..).collect()
                };
                // Untold: a repaint would only ask again and fail again.
                for job in waiting {
                    job.ticket.end(None, false);
                }
                return Err(ReportError::unsupported(format!(
                    "Preview could not start drawing its pictures: {error}"
                )));
            }
        }
        Err(drawing_pictures())
    }

    /// Keeps `picture`, drawn here and taken, to be taken again: one its
    /// caller could not keep (past the static-picture budget) is not drawn
    /// again on every repaint.
    fn keep(
        &self,
        key: ApartKey,
        image: &LibraryImage,
        picture: Result<Arc<Picture>, ReportError>,
    ) {
        let mut owner = lock(&self.0);
        if owner.drawn.len() >= APART_DRAWN {
            owner.drawn.pop_front();
        }
        owner.drawn.push_back((key, (image.clone(), picture)));
    }

    /// Calls `notify` after each drawing ends, once one is set.
    fn on_drawn(&self, notify: impl FnOnce() -> Arc<dyn Fn() + Send + Sync>) {
        let mut owner = lock(&self.0);
        if owner.notify.is_none() {
            owner.notify = Some(notify());
        }
    }

    /// Throws away what is drawn and what was asked for: a drawing asked
    /// before is not kept, and the same picture asked for again is drawn
    /// anew rather than waiting for it.
    fn clear(&self) {
        let mut owner = lock(&self.0);
        owner.generation = owner.generation.wrapping_add(1);
        owner.drawn.clear();
        owner.pending.clear();
    }
}

/// `mutex`'s value, also after a thread panicked holding it: the maps it
/// guards stay whole.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The error a page gives while one of its SVG pictures is being drawn
/// apart: pending (code -1, as for data still loading), so Preview says so
/// and draws the page again later.
fn drawing_pictures() -> ReportError {
    ReportError {
        code: -1,
        message: "Loading report pictures…".into(),
    }
}
/// A library picture's key, and for SVG the raster size in pixels it is
/// drawn at.
type PictureKey = (u64, Option<[u32; 2]>);
type DecodedPicture = (LibraryImage, Result<Arc<Picture>, ReportError>);
/// Container data's MD5, its frame's size in hundredths of a point and its
/// graphic format.
type ContainerKey = ([u8; 16], [u64; 2], u16);
/// The picture and where it goes in the frame; `None` for data that does
/// not decode.
type ContainerPicture = Option<(Arc<Picture>, [f64; 4])>;
/// A button icon's library key, tint and side in pixels.
type IconKey = (u64, Option<[u8; 4]>, u32);
impl std::fmt::Debug for Pictures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set()
            .entries(self.0.borrow().keys())
            .entries(self.1.borrow().keys())
            .entries(self.2.borrow().keys())
            .finish()
    }
}

/// Icon pixels per point: the side of an icon's raster is this many times
/// its printed side (288 pixels an inch), so it stays sharp on paper.
const ICON_SCALE: f64 = 4.0;

/// An SVG icon tinted as Browse tints it (`document::tinted_image`) and
/// drawn as Browse draws it (in the SVG worker, what it does not draw left
/// out) into `side` x `side` pixels, stretched as Browse stretches it into
/// its square.
fn rasterize_icon(
    key: u64,
    image: &LibraryImage,
    tint: Option<egui::Color32>,
    side: u32,
) -> Result<Picture, ReportError> {
    let data = if image.data.len() as u64 > static_picture::MAX_BYTES {
        Err("SVG input exceeds the picture limit".to_owned())
    } else {
        Ok(crate::document::tinted_image(image, tint))
    };
    data.and_then(|data| {
        static_picture::svg(&data, Some([side, side]), svg_worker::Unsupported::Skip)
    })
    .map_err(|error| {
        ReportError::unsupported(format!(
            "Library image {key} could not be drawn for the report: {error}"
        ))
    })
}

/// A button's icon square in `bounds`, as Browse places it: its side the
/// icon size, at most the frame's; alone in the middle of the frame, or,
/// with a label, centered 6 points plus half its side from the left edge,
/// the label then starting its side plus 10 points from that edge.
fn icon_square(object: &LayoutObject, bounds: &Rect, label: &str) -> Option<Rect> {
    let button = object.button.as_ref().filter(|b| b.library_key != 0)?;
    let side = f64::from(button.icon_size.max(1))
        .min(bounds.width())
        .min(bounds.height())
        .max(0.0);
    let x = if label.is_empty() {
        (bounds.left + bounds.right) / 2.0
    } else {
        bounds.left + side / 2.0 + 6.0
    };
    let y = (bounds.top + bounds.bottom) / 2.0;
    Some(Rect {
        left: x - side / 2.0,
        right: x + side / 2.0,
        top: y - side / 2.0,
        bottom: y + side / 2.0,
        ..Default::default()
    })
}
impl Pictures {
    /// Preview's pictures: an SVG picture or icon not drawn yet is drawn in
    /// the SVG worker from the session's drawing thread ([`Drawing`]), which
    /// can take seconds (`svg_worker`), and the page is pending
    /// ([`drawing_pictures`]) until it is, so the window keeps painting and
    /// taking input meanwhile. PDF and Print draw in line
    /// ([`Pictures::default`]).
    pub fn drawn_apart() -> Pictures {
        Pictures(
            Default::default(),
            Default::default(),
            Default::default(),
            Some(Drawing::default()),
        )
    }

    pub fn clear(&mut self) {
        self.0.get_mut().clear();
        self.1.get_mut().clear();
        self.2.get_mut().clear();
        if let Some(drawing) = &self.3 {
            drawing.clear();
        }
    }

    /// Has `notify` made (once) and called after each picture drawn apart
    /// is ready: Preview asks for a repaint then, rather than polling.
    pub fn on_drawn(&self, notify: impl FnOnce() -> Arc<dyn Fn() + Send + Sync>) {
        if let Some(drawing) = &self.3 {
            drawing.on_drawn(notify);
        }
    }

    /// Button `object`'s icon, library image `key`, `side` pixels square:
    /// an SVG tinted and rasterized as Browse paints it, a PNG or JPEG
    /// decoded as a picture.
    fn icon(
        &self,
        layout: &Layout,
        object: &LayoutObject,
        key: u64,
        tint: Option<Color>,
        side: u32,
    ) -> Result<Arc<Picture>, ReportError> {
        let image = layout.library_images.get(&key).ok_or_else(|| {
            ReportError::unsupported(format!(
                "The icon of report button {} (library image {key}) is missing",
                object.id
            ))
        })?;
        let tint = tint.map(crate::document::color);
        let cache_key = (key, tint.map(|c| c.to_array()), side);
        let mut decoded = self.2.borrow_mut();
        if let Some((source, picture)) = decoded.get(&cache_key) {
            if source == image {
                return picture.clone();
            }
        }
        // An icon past the SVG limit is refused in line, never copied.
        let apart = self
            .3
            .as_ref()
            .filter(|_| image.data.len() as u64 <= static_picture::MAX_BYTES);
        let picture = match (image.format, apart) {
            (LibraryImageFormat::Svg, Some(drawing)) => {
                match drawing.take_or_start(ApartKey::Icon(cache_key), image, move |image| {
                    rasterize_icon(key, image, tint, side).map(Arc::new)
                }) {
                    Err(error) if error.code == -1 => return Err(error),
                    drawn => drawn,
                }
            }
            (LibraryImageFormat::Svg, None) => rasterize_icon(key, image, tint, side).map(Arc::new),
            _ => decode_picture(key, image).map(Arc::new),
        };
        decoded.insert(cache_key, (image.clone(), picture.clone()));
        picture
    }

    /// What container data shows in a frame of `size` points: its picture
    /// and where the format places it ([left, top, width, height] in the
    /// frame), `Some(None)` for data that does not decode, `None` for data
    /// without a picture (a file).
    fn container(
        &self,
        data: &om_data::calc::Container,
        format: GraphicFormat,
        size: (f64, f64),
    ) -> Option<ContainerPicture> {
        let (_, bytes) = om_data::containers::picture_stream(data)?;
        let md5 = match &data.library {
            Some(library) => library.md5,
            None => om_data::containers::md5([(b"".as_slice(), bytes)]),
        };
        let hundredths = |v: f64| (v.max(0.0) * 100.0).round() as u64;
        let key = (md5, [hundredths(size.0), hundredths(size.1)], format.0);
        if let Some(shown) = self.1.borrow().get(&key) {
            return Some(shown.clone());
        }
        let shown = decode_container_picture(bytes, format, size);
        self.1.borrow_mut().insert(key, shown.clone());
        Some(shown)
    }

    fn get(&self, layout: &Layout, object: &LayoutObject) -> Result<Arc<Picture>, ReportError> {
        let key = object.graphic.as_ref().map_or(0, |g| g.library_key);
        let image = layout.library_images.get(&key).ok_or_else(|| {
            ReportError::unsupported(format!(
                "The picture of report object {} (library image {key}) is missing",
                object.id
            ))
        })?;
        let size = if image.format == LibraryImageFormat::Svg {
            let bounds = object
                .bounds
                .as_ref()
                .ok_or_else(|| ReportError::unsupported("A report SVG picture has no bounds"))?;
            Some(static_picture::raster_size(
                bounds.width(),
                bounds.height(),
            )?)
        } else {
            None
        };
        let cache_key = (key, size);
        let mut decoded = self.0.borrow_mut();
        if let Some((source, picture)) = decoded.get(&cache_key) {
            if source == image {
                return picture.clone();
            }
        }
        // A picture past the SVG limit is refused in line, never copied.
        let apart = self
            .3
            .as_ref()
            .filter(|_| image.data.len() as u64 <= static_picture::MAX_SVG_BYTES);
        let picture = match (apart, size) {
            (Some(drawing), Some(_)) => {
                match drawing.take_or_start(ApartKey::Picture(cache_key), image, move |image| {
                    static_picture::decode(key, image, size).map(Arc::new)
                }) {
                    Err(error) if error.code == -1 => return Err(error),
                    drawn => drawn,
                }
            }
            _ => static_picture::decode(key, image, size).map(Arc::new),
        };
        let retained: usize = decoded
            .values()
            .map(|(source, p)| {
                source
                    .data
                    .len()
                    .saturating_add(p.as_ref().map_or(0, |p| p.rgba.len()))
            })
            .sum();
        if retained
            .saturating_add(picture.as_ref().map_or(0, |p| p.rgba.len()))
            .saturating_add(image.data.len())
            > 512 << 20
        {
            // Drawn apart, it is kept there to be refused again, not drawn
            // again on every repaint.
            if let (Some(drawing), Some(_)) = (apart, size) {
                drawing.keep(ApartKey::Picture(cache_key), image, picture);
            }
            return Err(ReportError::unsupported(
                "Report static pictures exceed the 512 MiB static-picture cache budget",
            ));
        }
        decoded.insert(cache_key, (image.clone(), picture.clone()));
        picture
    }
}
#[derive(Clone, Debug)]
pub(crate) struct ReportPart {
    pub bounds: Rect,
    pub style: ResolvedStyle,
    pub objects: Vec<ReportObject>,
}
#[derive(Clone, Debug)]
pub(crate) struct ReportPage {
    pub parts: Vec<ReportPart>,
}

type SourceObject<'a> = (&'a LayoutObject, Rect, usize);

/// Independent coordinates for content evaluation. A once-only part or footer
/// may reuse a record on a different physical page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentContext {
    /// Index in the selected records, in found-set order. A sub-summary
    /// part's is its group's first (leading) or last (trailing) record.
    pub record: usize,
    /// Zero-based physical page, independent of the record's first placement.
    pub page: usize,
    /// A sub-summary part's break field: its summary fields read the
    /// record's group (`GetSummary`) rather than the whole found set.
    pub group: Option<BreakField>,
    /// The portal row an object prints in: its fields read the row's
    /// related record.
    pub portal: Option<PortalRowContext>,
}

/// `context` in portal row `row`, as Browse evaluates a portal row's
/// objects: the row's record of the portal's occurrence, and its number.
pub(crate) fn in_row(
    context: om_data::calc::eval::Context,
    row: Option<PortalRowContext>,
) -> om_data::calc::eval::Context {
    match row {
        Some(row) => context
            .with_portal(row.occurrence, row.record)
            .with_portal_row_number(row.number as u64),
        None => context,
    }
}

/// A portal row's related record: what Browse's portal rows evaluate in
/// (`Context::with_portal`, row number from 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRowContext {
    pub occurrence: u64,
    pub record: u64,
    pub number: usize,
}

/// What a report shows of its objects for a record: the text of a text,
/// button or field object, the data of a container field, and the
/// conditional formats that hold for it. A closure gives text only: its
/// container fields print as their text, and an object with conditional
/// formatting is refused rather than printed unformatted.
pub trait ReportContent {
    fn text(
        &mut self,
        object: &LayoutObject,
        context: ContentContext,
    ) -> Result<String, ReportError>;

    /// The data a field object shows when it is a container field holding
    /// data, its streams loaded; `None` for other objects and empty fields.
    /// A missing or pending value is an error, as for text.
    fn container(
        &mut self,
        _object: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Arc<om_data::calc::eval::StoredContainer>>, ReportError> {
        Ok(None)
    }

    /// The conditional formats of `object` whose condition holds for the
    /// record and page, as indexes into `calcs.conditions` in stored order;
    /// `None` when this source does not evaluate them.
    fn conditions(
        &mut self,
        _object: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Vec<usize>>, ReportError> {
        Ok(None)
    }

    /// The related records `portal` shows for the record in `context`, in
    /// the relationship's order; `None` when this source does not read
    /// them. A pending read is an error, as for text.
    fn portal_records(
        &mut self,
        _portal: &LayoutObject,
        _context: ContentContext,
    ) -> Result<Option<Vec<u64>>, ReportError> {
        Ok(None)
    }

    /// The panel tab control `control` shows in the window the report is
    /// for; `None` to print the one it first shows.
    fn front_panel(&mut self, _control: &LayoutObject) -> Option<u64> {
        None
    }
}

impl<F> ReportContent for F
where
    F: FnMut(&LayoutObject, ContentContext) -> Result<String, ReportError>,
{
    fn text(
        &mut self,
        object: &LayoutObject,
        context: ContentContext,
    ) -> Result<String, ReportError> {
        self(object, context)
    }
}

/// A found set's record ids and the sorted groups read for them.
pub(crate) type FoundSetGroups = (std::sync::Arc<[u64]>, Vec<BreakGroups>);

/// Grouping reads every found record's sort values once per found set, and
/// each sub-summary value reads its summary's found-set run; both are linear
/// in the found set, so grouped reports are bounded explicitly. Measuring
/// parts that sliding resizes is linear too and shares the bound.
pub(crate) const MAX_GROUPED_RECORDS: u64 = 10_000;

/// The layout's sub-summary break fields, checked against the calculation
/// schema: fields of the table `occurrence` shows that a sort can group by.
/// Empty without sub-summary parts, when no found-set data is needed.
pub(crate) fn report_breaks(
    layout: &Layout,
    schema: &om_data::host::CalcSchema,
    occurrence: u64,
) -> Result<Vec<BreakField>, ReportError> {
    let breaks = om_layout::pagination::report_breaks(layout).map_err(ReportError::unsupported)?;
    let table = schema.table_of(occurrence).map(|t| t.id);
    for field in &breaks {
        let def = schema
            .field(field.occurrence, field.field)
            .filter(|_| table.is_some() && schema.table_of(field.occurrence).map(|t| t.id) == table)
            .ok_or_else(|| {
                ReportError::unsupported(
                    "Sub-summary break fields must be fields of the layout's table",
                )
            })?;
        if def.summary.is_some() || def.value_type == om_data::calc::ValueType::Container {
            return Err(ReportError::unsupported(
                "Summary and container sub-summary break fields are unsupported",
            ));
        }
    }
    Ok(breaks)
}

/// A failure of `FileCalc::found_groups` as a report refusal.
pub(crate) fn group_error(error: om_data::host::GroupError) -> ReportError {
    use om_data::host::GroupError;
    ReportError {
        code: 3,
        message: match error {
            GroupError::RelatedSortKey => {
                "Sub-summary groups after a related-field sort key are unsupported".into()
            }
            GroupError::UnevaluableKey => {
                "A sub-summary group's sort key could not be calculated on every record".into()
            }
            GroupError::VariableKey => "Sub-summary groups by a calculation that may read \
                variables are unsupported: the records were not sorted with them"
                .into(),
            GroupError::Calc(error) => format!(
                "The report groups could not be read (error {})",
                error.code()
            ),
        },
    }
}

/// The groups of the found set at the `breaks` it is sorted by, for the
/// `selected` record ids in output order: the whole found set in its order,
/// or records of it (the current record keeps its group in the found set).
pub(crate) fn selected_groups(
    breaks: &[BreakField],
    found: &om_data::host::FoundGroups,
    selected: &[u64],
) -> Result<Vec<BreakGroups>, ReportError> {
    let stale = || ReportError::unsupported("The found set changed while grouping the report");
    let positions: Vec<usize> = if selected.len() == found.records.len() {
        if selected != &found.records[..] {
            return Err(stale());
        }
        (0..selected.len()).collect()
    } else {
        selected
            .iter()
            .map(|id| found.records.iter().position(|r| r == id).ok_or_else(stale))
            .collect::<Result<_, _>>()?
    };
    Ok(breaks
        .iter()
        .zip(&found.breaks)
        .filter_map(|(field, sorted)| {
            let (depth, groups) = sorted.as_ref()?;
            Some(BreakGroups {
                field: *field,
                depth: *depth,
                groups: positions.iter().map(|&p| groups[p]).collect(),
            })
        })
        .collect())
}
fn report_objects<'a>(
    layout: &'a Layout,
    paper: &PageSize,
    fronts: &BTreeMap<u64, u64>,
) -> Result<Vec<SourceObject<'a>>, ReportError> {
    walk_report_objects(layout, paper, fronts, &mut Err)
}

/// Layout Setup's "Delineate fields on current record only": bit 0 of the
/// layout's options (layout-model.md 4.2).
fn delineates_current_record_only(layout: &Layout) -> bool {
    layout.def.as_ref().is_some_and(|def| def.flags & 0x1 != 0)
}

/// The panel each tab control of `layout` shows in the window `content`
/// reports for ([`ReportContent::front_panel`]), by control.
fn chosen_panels(layout: &Layout, content: &mut impl ReportContent) -> BTreeMap<u64, u64> {
    (layout.objects.objects.iter())
        .filter(|o| o.kind() == ObjectKind::TabControl)
        .filter_map(|control| Some((control.id, content.front_panel(control)?)))
        .collect()
}

/// Every refusal [`report_objects`] would give for `layout`, not only the
/// first: each refused object's, then the layout's own; and the ids of the
/// objects it would print. A refused object's contents are not checked.
/// For tools that rank what blocks reports.
#[cfg(test)]
pub(crate) fn report_object_refusals(
    layout: &Layout,
    paper: &PageSize,
) -> (Vec<ReportError>, Vec<u64>) {
    let mut refusals = Vec::new();
    let placed = walk_report_objects(layout, paper, &BTreeMap::new(), &mut |error| {
        refusals.push(error);
        Ok(())
    })
    .map(|objects| objects.iter().map(|(object, _, _)| object.id).collect())
    .unwrap_or_default();
    (refusals, placed)
}

/// [`report_objects`], passing each refusal to `refuse`: an error it
/// returns ends the walk, otherwise the walk goes on without the object.
fn walk_report_objects<'a>(
    layout: &'a Layout,
    paper: &PageSize,
    fronts: &BTreeMap<u64, u64>,
    refuse: &mut dyn FnMut(ReportError) -> Result<(), ReportError>,
) -> Result<Vec<SourceObject<'a>>, ReportError> {
    let mut objects: Vec<SourceObject<'a>> = Vec::new();
    let mut visited = BTreeSet::new();
    // Match Browse: roots define stacking, and each object is followed by
    // its contents, depth first in `childkey` order; unattached top-level
    // objects follow in storage order. Each start is walked with an
    // explicit stack, and an object is reached once, however its
    // containers list it.
    let starts = layout
        .objects
        .root
        .iter()
        .filter_map(|id| layout.objects.get(*id))
        .chain(layout.objects.objects.iter().filter(|o| o.parent.is_none()));
    // A child another container claimed is reached there.
    let contents = |object: &'a LayoutObject| {
        object
            .children
            .iter()
            .rev()
            .filter_map(|&id| layout.objects.get(id))
            .filter(move |child| child.parent == Some(object.id))
    };
    for start in starts {
        let mut stack = vec![(start, None)];
        while let Some((object, via)) = stack.pop() {
            if !visited.insert(object.id) {
                continue;
            }
            let placed = match report_object(layout, paper, object, via, &objects) {
                Ok(placed) => placed,
                Err(error) => {
                    refuse(error)?;
                    None
                }
            };
            match placed {
                Some(source) => {
                    objects.push(source);
                    // A tab control prints its front panel only, as Browse
                    // paints only the active one; the others are neither
                    // printed nor checked.
                    if object.kind() == ObjectKind::TabControl {
                        let front = front_panel(layout, object, fronts.get(&object.id).copied());
                        let control = objects.len() - 1;
                        for child in contents(object) {
                            if child.kind() == ObjectKind::TabPanel && Some(child.id) != front {
                                let mut skipped = vec![child];
                                while let Some(hidden) = skipped.pop() {
                                    if visited.insert(hidden.id) {
                                        skipped.extend(contents(hidden));
                                    }
                                }
                            } else {
                                stack.push((child, Some(control)));
                            }
                        }
                    }
                    // Only a group's, a portal's and a front tab panel's
                    // contents print (the others refuse); a portal's print
                    // in each row.
                    if matches!(
                        object.kind(),
                        ObjectKind::Group | ObjectKind::Portal | ObjectKind::TabPanel
                    ) {
                        let group = objects.len() - 1;
                        stack.extend(contents(object).map(|child| (child, Some(group))));
                    }
                }
                // A navigation part never prints, nor what its objects hold;
                // nor does what lies beyond the printable width. A refused
                // object's contents are not checked.
                None => {
                    let mut skipped: Vec<_> = contents(object).collect();
                    while let Some(child) = skipped.pop() {
                        if visited.insert(child.id) {
                            skipped.extend(contents(child));
                        }
                    }
                }
            }
        }
    }
    // What no walk reached is inside another kind of container (or a
    // containment cycle): refused, so nothing nested disappears silently.
    if layout
        .objects
        .objects
        .iter()
        .any(|object| !visited.contains(&object.id))
    {
        refuse(ReportError::unsupported(
            "Nested report objects are unsupported",
        ))?;
    }
    // Whether an object left out beyond the printable width still holds
    // what slides, or resizes its part, is unmeasured; such layouts are
    // refused.
    let part_of = |object: &LayoutObject| {
        let rect = layout.absolute_bounds(object.id)?;
        layout.parts.iter().position(|p| {
            rect.top >= p.top
                && rect.top < p.bottom()
                && !matches!(
                    p.kind(),
                    PartKind::TopNavigation | PartKind::BottomNavigation
                )
        })
    };
    let beyond: BTreeSet<usize> = (layout.objects.objects.iter())
        .filter(|o| {
            layout
                .absolute_bounds(o.id)
                .is_some_and(|r| beyond_page(o, &r, paper))
        })
        .filter_map(part_of)
        .collect();
    // Likewise whether an object hidden when printing still holds back
    // what slides up below it.
    let hidden: BTreeSet<usize> = (layout.objects.objects.iter())
        .filter(|o| o.flags & sliding::NON_PRINTING != 0)
        .filter_map(part_of)
        .collect();
    for (parts, what) in [
        (&beyond, "objects beyond the printable width"),
        (&hidden, "objects hidden when printing"),
    ] {
        // Only what prints can slide: a slider that is itself left out (or
        // inside what is) moves nothing.
        if let Some((object, _, _)) = objects.iter().find(|(o, _, _)| {
            part_of(o).is_some_and(|part| {
                parts.contains(&part) && slide_in_part(layout, o, part).is_some()
            })
        }) {
            refuse(ReportError::unsupported(format!(
                "Report object {} slides up in a part with {what}; how those slide is \
                 unmeasured",
                object.id
            )))?;
        }
    }
    // Whether a sliding object overlapping another would move over, under or
    // out of it natively is unmeasured; such layouts are refused.
    for (i, (a, ra, pa)) in objects.iter().enumerate() {
        for (b, rb, pb) in &objects[i + 1..] {
            if pa == pb
                && (slide_in_part(layout, a, *pa).is_some()
                    || slide_in_part(layout, b, *pb).is_some())
                && ra.left < rb.right - SLIDE_EPSILON
                && rb.left < ra.right - SLIDE_EPSILON
                && ra.top < rb.bottom - SLIDE_EPSILON
                && rb.top < ra.bottom - SLIDE_EPSILON
            {
                refuse(ReportError::unsupported(format!(
                    "Report objects {} and {} overlap and one slides up; \
                     overlapping sliding objects are unsupported",
                    a.id, b.id
                )))?;
            }
        }
    }
    Ok(objects)
}

/// The panel a tab control prints: the one its window shows (`chosen`),
/// as FileMaker prints the front tab; else its default panel, else its
/// current one, else its first, as Browse first shows it
/// (`document::default_panel`).
fn front_panel(layout: &Layout, control: &LayoutObject, chosen: Option<u64>) -> Option<u64> {
    let panels: Vec<u64> = (control.children.iter())
        .filter(|id| {
            layout
                .objects
                .get(**id)
                .is_some_and(|o| o.kind() == ObjectKind::TabPanel && o.parent == Some(control.id))
        })
        .copied()
        .collect();
    let tab = control.tab.as_ref();
    [
        chosen.unwrap_or(0),
        tab.map_or(0, |t| t.default_panel_key),
        tab.map_or(0, |t| t.current_panel_key),
    ]
    .into_iter()
    .find(|key| panels.contains(key))
    .or_else(|| panels.first().copied())
}

/// Tab control `control`'s tabs at `rect` (its page bounds), as Browse
/// draws them (`document::tab_headers`): each panel's tab at its stored
/// left edge and width, the control's tab height high, in the panel's style
/// (state 7 for the front tab, 1 for the others), its label centered in
/// the panel's text style, else the control's size and color.
fn tab_headers(
    layout: &Layout,
    control: &LayoutObject,
    rect: &Rect,
    (index, front): (usize, Option<u64>),
    laid: &mut Vec<LaidOutObject>,
) -> Result<(), ReportError> {
    let height = f64::from(control.tab.as_ref().map_or(30, |tab| tab.tab_height.max(1)));
    let mut left = rect.left;
    for panel in (control.children.iter()).filter_map(|id| layout.objects.get(*id)) {
        if panel.kind() != ObjectKind::TabPanel || panel.parent != Some(control.id) {
            continue;
        }
        let info = panel.tab_panel.as_ref();
        let width = f64::from(info.map_or(100, |tab| tab.tab_width.max(1)));
        let x = info.map_or(left, |tab| rect.left + f64::from(tab.tab_left_edge));
        let bounds = Rect {
            left: x,
            top: rect.top,
            right: x + width,
            bottom: rect.top + height,
            ..Default::default()
        };
        let state = if Some(panel.id) == front { 7 } else { 1 };
        let mut text = crate::document::text_style(layout, panel, &[], state);
        let control_text = crate::document::text_style(layout, control, &[], state);
        text.font_size = text.font_size.or(control_text.font_size);
        text.color = text.color.or(control_text.color);
        text.text_align = Some(2);
        text.vertical_align = Some(1);
        let label = panel.text.as_deref().unwrap_or("");
        // Rounded as the panel's style rounds it, as Browse draws a tab.
        let radius = corner_radius(
            |id| layout.object_property(panel, 1, state, id),
            &format!("Report tab {}", panel.id),
        )?;
        laid.push(LaidOutObject {
            index,
            text: prepare_text(label, &bounds, &text)?.0,
            bounds,
            conditions: Vec::new(),
            icon: None,
            fill: Some((
                crate::document::formatted_style(layout, panel, &[], 1, state),
                radius,
            )),
            container: None,
        });
        left = x + width;
    }
    Ok(())
}

/// A portal's rows, as Browse lays them out (`document::portals`): inside
/// the frame's borders, each its height over its number of rows. Its first
/// row shows the related record its initial row names.
struct PortalGeometry {
    /// The frame inside its borders.
    viewport: Rect,
    row_height: f64,
    /// Index of the related record in the first row.
    first: usize,
    /// The rows the viewport shows.
    rows: usize,
    /// What a first-row object moves by in the first row: the frame's
    /// left and top border widths.
    offset: (f64, f64),
}

fn portal_geometry(layout: &Layout, portal: &LayoutObject, rect: &Rect) -> Option<PortalGeometry> {
    let options = portal.portal.as_ref()?;
    let style = layout.object_style(portal, 1, 1);
    // As Browse draws a border: a positive width that is not style 0 or 1.
    let border = |side: usize| {
        style.border_width[side]
            .filter(|w| {
                w.is_finite() && *w > 0.0 && !matches!(style.border_style[side], Some(0 | 1))
            })
            .unwrap_or(0.0)
            .min(1000.0)
    };
    let viewport = Rect {
        left: rect.left + border(3),
        top: rect.top + border(0),
        right: rect.right - border(1),
        bottom: rect.bottom - border(2),
        ..Default::default()
    };
    if !(viewport.width() > 0.0 && viewport.height() > 0.0) {
        return None;
    }
    let count = options.num_rows.clamp(1.0, 10_000.0);
    let row_height = ((rect.height() - border(0) - border(2)).max(1.0) / count).max(1.0);
    Some(PortalGeometry {
        // Less a rounding error: a frame of exactly n rows shows n, not n
        // and a sliver of another.
        rows: (viewport.height() / row_height - 1e-6)
            .ceil()
            .clamp(1.0, 10_000.0) as usize,
        row_height,
        first: options.initial_row.saturating_sub(1).max(0) as usize,
        offset: (border(3), border(0)),
        viewport,
    })
}

/// Claris: only objects within the margins print. One wholly beyond the
/// printable width is left out with what it holds; the part's clip cuts one
/// that crosses it (inferred). A line on the edge is not beyond it: half its
/// stroke is inside.
fn beyond_page(object: &LayoutObject, rect: &Rect, paper: &PageSize) -> bool {
    let width = paper.printable_width();
    rect.left > width || (rect.left == width && object.kind() != ObjectKind::Line)
}

/// One object's place in the report: its absolute bounds and part, after
/// the checks every report object passes. `via` is the index in `objects`
/// of the group the object was reached through, `None` for a top-level
/// object. `None` for an object hidden when printing, in a navigation part
/// or wholly beyond the printable width, which never prints (and neither
/// do its contents).
fn report_object<'a>(
    layout: &'a Layout,
    paper: &PageSize,
    object: &'a LayoutObject,
    via: Option<usize>,
    objects: &[SourceObject<'a>],
) -> Result<Option<SourceObject<'a>>, ReportError> {
    // Hide when printing leaves the object, and what it holds, out of
    // every printed page (Claris, "Removing blank spaces in printouts";
    // layout-model.md 5.8), whatever else it is set to do or wherever it
    // is: nothing else about it is checked.
    if object.flags & sliding::NON_PRINTING != 0 {
        return Ok(None);
    }
    // Whether a tab control draws the caption of a panel hidden when
    // printing is unmeasured.
    if object.kind() == ObjectKind::TabControl
        && (object.children.iter())
            .filter_map(|id| layout.objects.get(*id))
            .any(|panel| panel.flags & sliding::NON_PRINTING != 0)
    {
        return Err(ReportError::unsupported(format!(
            "Report tab control {} has a panel hidden when printing; how its tab prints is \
             unmeasured",
            object.id
        )));
    }
    let rect = layout
        .absolute_bounds(object.id)
        .ok_or_else(|| ReportError::unsupported("A report object has no geometry"))?;
    let part = layout
        .parts
        .iter()
        .position(|p| rect.top >= p.top && rect.top < p.bottom())
        .ok_or_else(|| ReportError::unsupported("An object is outside the report parts"))?;
    let group = via.map(|index| &objects[index]);
    let in_portal = group.filter(|(container, _, _)| container.kind() == ObjectKind::Portal);
    match (object.parent, group) {
        (None, None) => {}
        // A group's member prints with its group, in the group's part.
        (Some(parent), Some((container, _, container_part)))
            if parent == container.id && part == *container_part => {}
        (Some(_), Some(_)) => {
            return Err(ReportError::unsupported(format!(
                "Report object {} is not in its group's part",
                object.id
            )))
        }
        _ => {
            return Err(ReportError::unsupported(
                "Nested report objects are unsupported",
            ))
        }
    }
    if group.is_none()
        && matches!(
            layout.parts[part].kind(),
            PartKind::TopNavigation | PartKind::BottomNavigation
        )
    {
        return Ok(None);
    }
    if beyond_page(object, &rect, paper) {
        return Ok(None);
    }
    if object.flags & sliding::LEFT != 0 {
        return Err(ReportError::unsupported(format!(
            "Report object {} slides left; sliding left is unsupported",
            object.id
        )));
    }
    // A portal's rows have its height; how its objects would slide in a
    // row, or a tab control, is unmeasured.
    let slides = slide_in_part(layout, object, part).is_some();
    if slides
        && (in_portal.is_some()
            || matches!(
                object.kind(),
                ObjectKind::Portal | ObjectKind::TabControl | ObjectKind::TabPanel
            ))
    {
        return Err(ReportError::unsupported(format!(
            "Report object {} slides up in a portal; sliding portals is unsupported",
            object.id
        )));
    }
    // How FileMaker slides a group, or one of its members, is unmeasured.
    if slides && (group.is_some() || object.kind() == ObjectKind::Group) {
        return Err(ReportError::unsupported(format!(
            "Report object {} slides up in a group; sliding groups is unsupported",
            object.id
        )));
    }
    // Pages are planned without conditions, so one that resized a sliding
    // object's text would not be measured.
    if slides && !object.calcs.conditions.is_empty() {
        return Err(ReportError::unsupported(format!(
            "Report object {} slides up and has conditional formatting; \
             sliding formatted objects are unsupported",
            object.id
        )));
    }
    if !(matches!(
        object.kind(),
        ObjectKind::Text
            | ObjectKind::Field
            | ObjectKind::Rectangle
            | ObjectKind::Line
            | ObjectKind::Oval
            | ObjectKind::Graphic
            | ObjectKind::Button
            | ObjectKind::Group
            | ObjectKind::Portal
            | ObjectKind::TabControl
            | ObjectKind::TabPanel
    ) || is_web_viewer(object))
        || (in_portal.is_some()
            && (matches!(
                object.kind(),
                ObjectKind::Group | ObjectKind::Portal | ObjectKind::TabControl
            ) || is_web_viewer(object)))
        // A panel is a tab control's, and a tab control holds panels only.
        || (object.kind() == ObjectKind::TabPanel)
            != group.is_some_and(|(container, _, _)| container.kind() == ObjectKind::TabControl)
    {
        return Err(ReportError::unsupported(format!(
            "Unsupported report object or calculation: {} ({:?})",
            object.id,
            object.kind()
        )));
    }
    let options = object.flags & !printed_flags(object.kind());
    let unsupported = if object.rotation != 0 {
        Some("rotation".to_owned())
    } else if options != 0 {
        Some(format!("object options {options:#x}"))
    } else if object.calcs.hide.is_some() {
        Some("a hide condition".to_owned())
    } else if object.calcs.label.is_some() {
        Some("a label calculation".to_owned())
    } else if !object.calcs.conditions.is_empty()
        && !matches!(
            object.kind(),
            ObjectKind::Text | ObjectKind::Field | ObjectKind::Button
        )
    {
        Some("conditional formatting".to_owned())
    } else if object.has_undecodable_calcs() {
        Some("calculations OpenMaker cannot read".to_owned())
    } else {
        None
    };
    if let Some(what) = unsupported {
        return Err(ReportError::unsupported(format!(
            "Unsupported report object or calculation: {} ({:?}), {what}",
            object.id,
            object.kind()
        )));
    }
    // A button prints as Browse draws it without an icon: its frame and its
    // label. An icon, contents, or a field this version does not decode
    // could change that appearance; so could a group's undecoded field.
    match object.kind() {
        ObjectKind::Button => match &object.button {
            // Display type 1, "Image only", prints its icon alone and
            // centered; 4, "Icon at left, text at right", left of the label.
            // The icon-above, icon-below and icon-right placements are not
            // drawn (layout-model.md 5.6.1).
            Some(button) if button.library_key != 0 && !matches!(button.display_type, 1 | 4) => {
                return Err(ReportError::unsupported(format!(
                    "Report button {} places its icon by display type {}; only display types 1 \
                     (icon alone) and 4 (icon left of the label) print",
                    object.id, button.display_type
                )))
            }
            Some(button) if button.other.is_empty() && object.children.is_empty() => {}
            _ => {
                return Err(ReportError::unsupported(format!(
                    "Unsupported report button options: {}",
                    object.id
                )))
            }
        },
        ObjectKind::Group if object.group.as_ref().is_none_or(|g| !g.other.is_empty()) => {
            return Err(ReportError::unsupported(format!(
                "Unsupported report group options: {}",
                object.id
            )))
        }
        ObjectKind::Group => {}
        ObjectKind::TabControl
            if object
                .tab
                .as_ref()
                .is_none_or(|tab| !tab.other.is_empty() || tab.tab_height < 1) =>
        {
            return Err(ReportError::unsupported(format!(
                "Unsupported report tab control options: {}",
                object.id
            )))
        }
        ObjectKind::TabPanel
            if object
                .tab_panel
                .as_ref()
                .is_none_or(|panel| !panel.other.is_empty() || panel.tab_width < 1) =>
        {
            return Err(ReportError::unsupported(format!(
                "Unsupported report tab panel options: {}",
                object.id
            )))
        }
        ObjectKind::Portal => match &object.portal {
            // Browse orders a sorted portal's rows (flag 0x8 turns its
            // stored order on) and filters them; that is not read here yet.
            Some(portal)
                if object.calcs.portal_filter.is_some()
                    || (portal.flags & 0x8 != 0 && object.calcs.portal_sort.is_some()) =>
            {
                return Err(ReportError::unsupported(format!(
                    "Report portal {} sorts or filters its rows; sorted and filtered portals \
                     are unsupported",
                    object.id
                )))
            }
            // A portal of the layout's own table shows the found set, which
            // a report's record does not read.
            Some(portal)
                if portal.other.is_empty()
                    && portal.num_rows.is_finite()
                    && portal.num_rows >= 1.0
                    && om_layout::occurrence_id(portal.table_alias_key).is_some_and(
                        |related| {
                            Some(related)
                                != (layout.def.as_ref())
                                    .and_then(|d| om_layout::occurrence_id(d.table_alias_key))
                        },
                    ) => {}
            _ => {
                return Err(ReportError::unsupported(format!(
                    "Unsupported report portal options: {}",
                    object.id
                )))
            }
        },
        _ => {}
    }
    // A portal's objects repeat in each row: they must lie in its first
    // row, inside its frame.
    if let Some((portal, portal_rect, _)) = in_portal {
        let fits = portal_geometry(layout, portal, portal_rect).is_some_and(|rows| {
            let (left, top) = rows.offset;
            rect.left + left >= rows.viewport.left - 0.01
                && rect.right + left <= rows.viewport.right + 0.01
                && rect.top + top >= rows.viewport.top - 0.01
                && rect.bottom + top <= rows.viewport.top + rows.row_height + 0.01
        });
        if !fits {
            return Err(ReportError::unsupported(format!(
                "Report object {} is outside its portal's first row",
                object.id
            )));
        }
    }
    // Browse paints only the library key's image and, like it, leaves
    // pictFormat to the stream's own signature. A mix color or undecoded
    // option might change the picture's appearance.
    if object.kind() == ObjectKind::Graphic
        && object
            .graphic
            .as_ref()
            .is_none_or(|g| g.library_key == 0 || g.mix_color != 0 || !g.other.is_empty())
    {
        return Err(ReportError::unsupported(format!(
            "Unsupported report picture options: {}",
            object.id
        )));
    }
    if matches!(object.kind(), ObjectKind::Text | ObjectKind::Button)
        && symbol_texts(object).any(|text| numbered(text, 0).is_none())
    {
        return Err(ReportError::unsupported(
            "Layout merge symbols other than {{PageNumber}} are unsupported",
        ));
    }
    if [rect.top, rect.left, rect.bottom, rect.right]
        .iter()
        .any(|n| !n.is_finite())
        || rect.left < 0.0
        || rect.width() < 0.0
        || rect.height() < 0.0
        || rect.bottom > layout.parts[part].bottom() + 0.01
    {
        return Err(ReportError::unsupported(
            "A report object crosses its part or page bounds",
        ));
    }
    Ok(Some((object, rect, part)))
}

/// The slide-up bits a report object may carry ([`sliding`]).
const SLIDE_UP_FLAGS: u64 = sliding::UP | sliding::RESIZE_PART | sliding::DIRECTLY_ABOVE;
/// `kTabOrderOmit` (layout-model.md 5.2.1, read by
/// `LayoutObject::GetTabOrderOmitted`): the object is left out of the tab
/// order, which nothing printed shows.
const TAB_ORDER_OMIT: u64 = 0x8;
/// `kLineStartLeft` and `kLineStartTop`: the corner a line starts at
/// (layout-model.md 5.3.1), drawn as Browse draws it. On other objects
/// the same bits are the undecoded table-view options, which refuse.
const LINE_START: u64 = 0x1000 | 0x2000;

/// The object flags a report prints `kind` with: anchors, the slide-up
/// bits, the tab order and tooltip bits, `kFixedPart` and `0x1`, and a
/// line's start corner. Any other bit refuses the report.
/// A web viewer (an external object with web viewer options); charts and
/// plug-in objects are external objects too, and stay refused.
fn is_web_viewer(object: &LayoutObject) -> bool {
    object.kind() == ObjectKind::External && object.web_viewer().is_some()
}

/// What prints over a web viewer: OpenMaker does not print web pages, so
/// the page says so instead of leaving a blank box (#278).
pub(crate) const WEB_VIEWER_CAPTION: &str = "Web viewer: OpenMaker does not print web pages.";

/// The caption plate over web viewer `bounds`, as Browse labels a viewer it
/// shows no page in (`document::paint_web_viewer_note`): 12 point caption
/// text centered on a light plate with a hairline border, 6 by 3 points of
/// padding, 4 points above the viewer's bottom edge. It is centered on the
/// part of the viewer within the printable width, so a viewer that crosses
/// the page edge still shows it.
fn web_viewer_caption(
    object: &LayoutObject,
    bounds: &Rect,
    paper: &PageSize,
) -> Result<ReportObject, ReportError> {
    const MARGIN: f64 = 4.0;
    let shade = |c: egui::Color32| Color {
        red: f32::from(c.r()) / 255.0,
        green: f32::from(c.g()) / 255.0,
        blue: f32::from(c.b()) / 255.0,
        alpha: 1.0,
    };
    let style = ResolvedStyle {
        background: Some(shade(crate::theme::MENU_FILL)),
        color: Some(shade(crate::theme::CAPTION)),
        font_size: Some(12.0),
        text_align: Some(2),
        padding: [Some(3.0), Some(6.0), Some(3.0), Some(6.0)],
        border_width: [Some(1.0); 4],
        border_color: [Some(shade(crate::theme::HAIRLINE)); 4],
        ..Default::default()
    };
    let (left, right) = (
        bounds.left.max(paper.margins.left),
        bounds
            .right
            .min(paper.margins.left + paper.printable_width()),
    );
    let too_small = |_| {
        ReportError::unsupported(format!(
            "Report web viewer {} is too small for its caption",
            object.id
        ))
    };
    let room = Rect {
        left: left + MARGIN,
        right: right - MARGIN,
        top: bounds.top + MARGIN,
        bottom: bounds.bottom - MARGIN,
        ..Default::default()
    };
    let (lines, height) = prepare_text(WEB_VIEWER_CAPTION, &room, &style).map_err(too_small)?;
    let pad = 2.0 * style.padding[1].unwrap_or(0.0);
    let width = lines
        .iter()
        .map(|line| line.glyphs.iter().map(|g| g.1).sum::<f64>())
        .fold(0.0, f64::max)
        + pad;
    let center = (left + right) / 2.0;
    let bottom = (bounds.bottom - MARGIN).max((bounds.top + bounds.bottom + height) / 2.0);
    let plate = Rect {
        left: center - width / 2.0,
        right: center + width / 2.0,
        top: bottom - height,
        bottom,
        ..Default::default()
    };
    let text = prepare_text(WEB_VIEWER_CAPTION, &plate, &style)
        .map_err(too_small)?
        .0;
    Ok(ReportObject {
        kind: ObjectKind::Rectangle,
        bounds: plate,
        style,
        text,
        picture: None,
        placed: None,
        file_icon: None,
        radius: 2.0,
        icon: None,
        line: None,
    })
}

fn printed_flags(kind: ObjectKind) -> u64 {
    let line = if kind == ObjectKind::Line {
        LINE_START
    } else {
        0
    };
    0xf000_0000 | 0x0002_4001 | TAB_ORDER_OMIT | SLIDE_UP_FLAGS | line
}
/// Tolerance, in points, for touching edges of sliding objects.
const SLIDE_EPSILON: f64 = 0.01;

/// Parts that can shrink and whose objects can slide. Claris: "Headers,
/// footers, title headers, title footers, and top navigation and bottom
/// navigation parts never shrink or slide up."
fn slides_in(kind: PartKind) -> bool {
    matches!(
        kind,
        PartKind::Body
            | PartKind::LeadingGrandSummary
            | PartKind::TrailingGrandSummary
            | PartKind::LeadingSubSummary
            | PartKind::TrailingSubSummary
    )
}

/// `object`'s slide-up option where it takes effect, in `layout`'s part
/// `part`: none in a part that never slides ([`slides_in`]), where an
/// object set to slide up prints where it is placed, as one that does not.
fn slide_in_part(
    layout: &Layout,
    object: &LayoutObject,
    part: usize,
) -> Option<om_layout::SlideUp> {
    object
        .slide_up()
        .filter(|_| layout.parts.get(part).is_some_and(|p| slides_in(p.kind())))
}

/// Whether a printed part's height depends on its record: an object there
/// slides up and also resizes its part. Such a report is measured record by
/// record before it is paginated; sliding alone moves objects within parts
/// of fixed height and needs no plan-time content.
pub(crate) fn resizes_parts(layout: &Layout) -> bool {
    layout.objects.objects.iter().any(|object| {
        !hidden_when_printing(layout, object)
            && object.slide_up().is_some_and(|slide| slide.resize_part)
            && layout.absolute_bounds(object.id).is_some_and(|rect| {
                layout
                    .parts
                    .iter()
                    .find(|p| rect.top >= p.top && rect.top < p.bottom())
                    .is_some_and(|p| slides_in(p.kind()))
            })
    })
}

/// Whether `object` or a container it is in is set to Hide when printing,
/// so that it never prints ([`report_object`]). Containers are followed at
/// most as many levels as the layout has objects, so a cycle ends.
fn hidden_when_printing(layout: &Layout, object: &LayoutObject) -> bool {
    let mut at = Some(object);
    for _ in 0..=layout.objects.objects.len() {
        let Some(object) = at else {
            return false;
        };
        if object.flags & sliding::NON_PRINTING != 0 {
            return true;
        }
        at = object.parent.and_then(|id| layout.objects.get(id));
    }
    false
}

/// In a sub-summary part, a summary field reads its group: the summary and
/// break field as `GetSummary` takes them. `None` for any other field, or
/// outside sub-summary parts. Summaries of another occurrence (a related
/// set) or a later repetition are refused there rather than approximated.
pub(crate) fn group_summary(
    def: &om_data::host::FieldDef,
    field: om_data::runtime::FieldRef,
    origin: u64,
    group: Option<BreakField>,
) -> Result<Option<(om_data::calc::eval::FieldId, om_data::calc::eval::FieldId)>, ReportError> {
    use om_data::calc::eval::FieldId;
    let Some(by) = group.filter(|_| def.summary.is_some()) else {
        return Ok(None);
    };
    if field.occurrence != origin || field.repetition != 1 {
        return Err(ReportError::unsupported(
            "Sub-summary parts support summary fields of the layout's own table occurrence only",
        ));
    }
    Ok(Some((
        FieldId {
            occurrence: field.occurrence,
            field: field.field,
        },
        FieldId {
            occurrence: by.occurrence,
            field: by.field,
        },
    )))
}

/// The fields `object` shows itself: a field object's field (repetition
/// `u32::MAX` when it shows several) and the fields its merge fields name,
/// unqualified names in `origin`. Names the schema lacks are left out.
pub(crate) fn shown_fields(
    object: &LayoutObject,
    schema: &om_data::host::CalcSchema,
    origin: u64,
) -> Vec<om_data::runtime::FieldRef> {
    use om_data::formula::Names;
    use om_data::runtime::FieldRef;
    use om_layout::TextRun;
    let mut shown = Vec::new();
    if let Some(field) = object
        .field_obj
        .as_ref()
        .filter(|_| object.kind() == ObjectKind::Field)
    {
        let spec = field.field_spec.as_ref();
        if let Some((occurrence, id)) = spec
            .and_then(|spec| Some((om_layout::occurrence_id(spec.table_key)?, spec.field_id()?)))
        {
            let repetition = if field.num_of_reps > 1 {
                // Its later repetitions show too.
                u32::MAX
            } else {
                spec.and_then(|spec| u32::try_from(spec.repetition.max(1)).ok())
                    .unwrap_or(u32::MAX)
            };
            shown.push(FieldRef {
                occurrence,
                field: id,
                repetition,
            });
        }
    }
    let names = schema.names();
    for run in &object.text_runs {
        let TextRun::MergeField(name) = run else {
            continue;
        };
        let (occurrence, name) = match name.split_once("::") {
            Some((to, name)) => match names.find_occurrence(to) {
                Some(occurrence) => (occurrence, name),
                None => continue,
            },
            None => (origin, name.as_str()),
        };
        let reference = match names.find_field(occurrence, name) {
            Some(field) => Some((field, 1)),
            None => name
                .strip_suffix(']')
                .and_then(|name| name.rsplit_once('['))
                .and_then(|(name, repetition)| {
                    Some((
                        names.find_field(occurrence, name)?,
                        repetition.parse::<u32>().ok().filter(|r| *r > 0)?,
                    ))
                }),
        };
        if let Some((field, repetition)) = reference {
            shown.push(FieldRef {
                occurrence,
                field,
                repetition,
            });
        }
    }
    shown
}

/// Why Browse List view cannot show sub-summary part `part` with its
/// group's values, or `None`. Every summary field the part shows, as a
/// field or a merge field ([`shown_fields`]), must be one [`group_summary`]
/// reads for the group. Such an object may not have layout calculations:
/// their `Self` would read the found set's value, not the group's. Objects
/// in portal rows keep their own related context and are not checked;
/// popovers do not show in a sub-summary band.
pub(crate) fn band_refusal(
    layout: &Layout,
    part: &Part,
    schema: &om_data::host::CalcSchema,
    origin: u64,
) -> Option<String> {
    let group = match part.break_field() {
        Ok(Some(field)) => field,
        Ok(None) => return Some("A sub-summary part has no break field".into()),
        Err(error) => return Some(error),
    };
    let mut pending: Vec<u64> = layout
        .objects
        .objects
        .iter()
        .filter(|object| {
            object.parent.is_none()
                && object
                    .bounds
                    .as_ref()
                    .is_some_and(|b| b.top >= part.top && b.top < part.bottom())
        })
        .map(|object| object.id)
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(object) = layout.objects.get(id) else {
            continue;
        };
        if matches!(object.kind(), ObjectKind::Portal | ObjectKind::Popover) {
            continue;
        }
        for field in shown_fields(object, schema, origin) {
            let Some(def) = schema
                .field(field.occurrence, field.field)
                .filter(|def| def.summary.is_some())
            else {
                continue;
            };
            if let Err(error) = group_summary(def, field, origin, Some(group)) {
                return Some(error.message);
            }
            let calcs = &object.calcs;
            if calcs.hide.is_some()
                || calcs.label.is_some()
                || calcs.tooltip.is_some()
                || calcs.placeholder.is_some()
                || calcs.active_segment.is_some()
                || !calcs.conditions.is_empty()
                || object.has_undecodable_calcs()
            {
                return Some(
                    "Summary fields with layout calculations in sub-summary parts are unsupported"
                        .into(),
                );
            }
        }
        pending.extend(object.children.iter().copied());
    }
    None
}

/// One printed part instance: its objects in stacking order, and its
/// printed height.
struct LaidOutPart {
    objects: Vec<LaidOutObject>,
    height: f64,
}

/// One printed object of a [`LaidOutPart`].
struct LaidOutObject {
    /// Index into the part's objects.
    index: usize,
    /// Page bounds.
    bounds: Rect,
    text: Vec<TextLine>,
    /// The conditional formats that hold for it.
    conditions: Vec<usize>,
    /// A button's icon square ([`icon_square`]).
    icon: Option<Rect>,
    /// A portal row's background or a tab: drawn as a rectangle of this
    /// style, with `text`, instead of the object `index` names.
    fill: Option<(ResolvedStyle, f64)>,
    /// A container field's data.
    container: Option<Arc<om_data::calc::eval::StoredContainer>>,
}

/// A part's objects in stacking order, with what their geometry and slide
/// options decide before any content: computed once per part, not per
/// record.
struct PartObjects<'s, 'a> {
    objects: Vec<&'s SourceObject<'a>>,
    /// Part-relative bounds.
    rects: Vec<Rect>,
    /// Top to bottom; ties keep stacking order, so a zero-height line at a
    /// field's top edge is above it.
    order: Vec<usize>,
    /// For each sliding object, the nearest objects above it that it slides
    /// against: all objects above, or only those directly above
    /// (horizontally overlapping), without any that another of them
    /// separates from it.
    nearest: Vec<Vec<usize>>,
    /// For each sliding object, every object above it that horizontally
    /// overlaps it: it never rises over one of them.
    colliders: Vec<Vec<usize>>,
    /// The lowest objects that resize the part: no other resizing object is
    /// below them.
    resizers: Vec<usize>,
    /// For each object in a portal's rows, the portal's index.
    in_portal: Vec<Option<usize>>,
    /// Each object's slide-up option where it takes effect: none in a part
    /// that never slides ([`slide_in_part`]).
    slides: Vec<Option<om_layout::SlideUp>>,
}

fn part_objects<'s, 'a>(
    objects: &'s [SourceObject<'a>],
    index: usize,
    part: &Part,
) -> PartObjects<'s, 'a> {
    let objects: Vec<_> = objects.iter().filter(|(_, _, p)| *p == index).collect();
    let slides: Vec<_> = (objects.iter())
        .map(|(object, _, _)| object.slide_up().filter(|_| slides_in(part.kind())))
        .collect();
    let rects: Vec<Rect> = objects
        .iter()
        .map(|(_, r, _)| Rect {
            top: r.top - part.top,
            bottom: r.bottom - part.top,
            left: r.left,
            right: r.right,
            ..Default::default()
        })
        .collect();
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&rects[a], &rects[b]);
        ra.top
            .total_cmp(&rb.top)
            .then(ra.bottom.total_cmp(&rb.bottom))
            .then(a.cmp(&b))
    });
    let mut rank = vec![0; rects.len()];
    for (position, &i) in order.iter().enumerate() {
        rank[i] = position;
    }
    // `a` is wholly above `b`, earlier in the order.
    let above =
        |a: usize, b: usize| rank[a] < rank[b] && rects[a].bottom <= rects[b].top + SLIDE_EPSILON;
    let overlaps = |a: usize, b: usize| {
        rects[a].left < rects[b].right - SLIDE_EPSILON
            && rects[b].left < rects[a].right - SLIDE_EPSILON
    };
    let mut nearest = vec![Vec::new(); rects.len()];
    let mut colliders = vec![Vec::new(); rects.len()];
    for o in 0..objects.len() {
        let Some(slide) = slides[o] else {
            continue;
        };
        let candidates: Vec<usize> = (0..rects.len())
            .filter(|&c| above(c, o) && (!slide.directly_above || overlaps(c, o)))
            .collect();
        nearest[o] = candidates
            .iter()
            .copied()
            .filter(|&c| !candidates.iter().any(|&b| b != c && above(c, b)))
            .collect();
        colliders[o] = (0..rects.len())
            .filter(|&c| above(c, o) && overlaps(c, o))
            .collect();
    }
    let resizing: Vec<usize> = (0..rects.len())
        .filter(|&i| slides[i].is_some_and(|s| s.resize_part))
        .collect();
    let resizers = resizing
        .iter()
        .copied()
        .filter(|&r| !resizing.iter().any(|&b| b != r && above(r, b)))
        .collect();
    let in_portal = (objects.iter())
        .map(|(object, _, _)| {
            let parent = object.parent?;
            (objects.iter()).position(|(o, _, _)| o.id == parent && o.kind() == ObjectKind::Portal)
        })
        .collect();
    PartObjects {
        objects,
        rects,
        order,
        nearest,
        colliders,
        resizers,
        in_portal,
        slides,
    }
}

/// `text` with each `{{PageNumber}}` (in any case, as Browse reads layout
/// symbols, `om_data::host::layout::merge_text`) replaced by page `page`'s
/// number (from 0): what FileMaker Pro 26.0.3 wrote on page *n* of Save
/// Records as PDF (calc-functions.md, O4). `None` when the text holds
/// another symbol, which reports refuse; an unclosed `{{` is text, as
/// Browse keeps it.
fn numbered(text: &str, page: usize) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail[2..].find("}}").map(|i| i + 2) else {
            out.push_str(tail);
            return Some(out);
        };
        if !tail[2..end].eq_ignore_ascii_case("PageNumber") {
            return None;
        }
        out.push_str(&(page + 1).to_string());
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    Some(out)
}

/// The texts a text object's or button's layout symbols are read from, as
/// Browse and the report's content read them: a merge text's literal runs,
/// each on its own (a symbol never spans a merged field), or else its label.
fn symbol_texts(object: &LayoutObject) -> Box<dyn Iterator<Item = &String> + '_> {
    use om_layout::TextRun;
    if object.text_runs.is_empty() {
        Box::new(object.text.iter())
    } else {
        Box::new(object.text_runs.iter().filter_map(|run| match run {
            TextRun::Literal(literal) => Some(literal),
            _ => None,
        }))
    }
}

/// `label` with each whole `{{PageNumber}}` (any case) replaced by page
/// `page`'s number (from 0): a merge text's label beside the runs its
/// symbols are read from ([`symbol_texts`]), for content that reads the
/// label. Its merged fields are written in it, so only whole symbols are
/// numbered, never one paired across a field, and a field's placeholder
/// (`<<Name>>`) is kept as written, whatever its name holds.
fn numbered_label(label: &str, page: usize) -> String {
    const SYMBOL: &str = "{{PageNumber}}";
    let mut out = String::with_capacity(label.len());
    let mut rest = label;
    loop {
        let field = rest.find("<<");
        let Some(start) = [field, rest.find("{{")].into_iter().flatten().min() else {
            break;
        };
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        // A merged field's placeholder is its name, kept as written.
        if field == Some(start) {
            let end = tail[2..].find(">>").map_or(2, |end| end + 4);
            out.push_str(&tail[..end]);
            rest = &tail[end..];
            continue;
        }
        match tail.get(..SYMBOL.len()) {
            Some(symbol) if symbol.eq_ignore_ascii_case(SYMBOL) => {
                out.push_str(&(page + 1).to_string());
                rest = &tail[SYMBOL.len()..];
            }
            _ => {
                out.push_str("{{");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `object` as page `page` (from 0) prints it: a text object's or a
/// button's `{{PageNumber}}` numbered where its symbols are read
/// ([`symbol_texts`], [`numbered`]), and a merge text's label too
/// ([`numbered_label`]), so every source reads it as plain text and no
/// field's value is touched; borrowed when it has none.
fn on_page(object: &LayoutObject, page: usize) -> std::borrow::Cow<'_, LayoutObject> {
    use om_layout::TextRun;
    if !matches!(object.kind(), ObjectKind::Text | ObjectKind::Button)
        || !symbol_texts(object).any(|text| text.contains("{{"))
    {
        return std::borrow::Cow::Borrowed(object);
    }
    let mut object = object.clone();
    let number = |text: &mut String| {
        if let Some(n) = numbered(text, page) {
            *text = n;
        }
    };
    if object.text_runs.is_empty() {
        if let Some(text) = object.text.as_mut() {
            number(text);
        }
    } else if let Some(text) = object.text.as_mut() {
        // The label the runs spell (each merged field as `<<name>>`) is
        // numbered as they are, run by run; one they do not spell is
        // numbered by whole symbols outside placeholders.
        let spelled = |numbered_runs: bool| {
            (object.text_runs.iter())
                .map(|run| match run {
                    TextRun::Literal(literal) if numbered_runs => {
                        numbered(literal, page).unwrap_or_else(|| literal.clone())
                    }
                    TextRun::Literal(literal) => literal.clone(),
                    TextRun::MergeField(name) => format!("<<{name}>>"),
                })
                .collect::<String>()
        };
        *text = if spelled(false) == *text {
            spelled(true)
        } else {
            numbered_label(text, page)
        };
    }
    for run in &mut object.text_runs {
        if let TextRun::Literal(literal) = run {
            number(literal);
        }
    }
    std::borrow::Cow::Owned(object)
}

/// Lay out one part for one record and page, sliding objects up as
/// FileMaker describes (Claris, "Removing blank spaces in printouts"):
///
/// - A field set to slide up shrinks to the height its text needs; an empty
///   one collapses and prints nothing. Other objects slide but never shrink.
/// - An object set to slide up rises by the least that its nearest objects
///   above rose (moved plus shrank), so the gaps between them remain and a
///   column closes up as a whole. An object that does not slide rises by
///   nothing and holds what is below it; with nothing above, nothing moves.
///   No object rises over one above that horizontally overlaps it.
/// - The part's bottom rises by the least that its lowest resizing objects
///   rose. An object then below the part's new bottom refuses the report.
///
/// `origin` is the part's page position (left, top). Without `text`, only
/// shrinking fields read content: the height is all pagination needs.
fn layout_part(
    layout: &Layout,
    part: &PartObjects<'_, '_>,
    height: f64,
    origin: (f64, f64),
    context: ContentContext,
    text: bool,
    content: &mut impl ReportContent,
) -> Result<LaidOutPart, ReportError> {
    let count = part.objects.len();
    let mut values = Vec::with_capacity(count);
    let mut data = Vec::with_capacity(count);
    let mut shrink = vec![0.0; count];
    for (i, (object, _, _)) in part.objects.iter().enumerate() {
        let paged = on_page(object, context.page);
        let object = paged.as_ref();
        let shrinks = part.slides[i].is_some() && object.kind() == ObjectKind::Field;
        let value = if matches!(
            object.kind(),
            ObjectKind::Text | ObjectKind::Field | ObjectKind::Button
        ) && (text || shrinks)
            && part.in_portal[i].is_none()
        {
            // Conditions only format printed text; sliding objects have none.
            let conditions = match text && !object.calcs.conditions.is_empty() {
                false => Vec::new(),
                true => content.conditions(object, context)?.ok_or_else(|| {
                    ReportError::unsupported(format!(
                        "Report object {}'s conditional formatting was not evaluated",
                        object.id
                    ))
                })?,
            };
            let style = text_box_style(layout, object, &conditions)?;
            // An "Image only" button prints its icon alone, centered
            // (layout-model.md 5.6.1): its stored label is not read.
            let value = match object.shows_label() {
                true => content.text(object, context)?,
                false => String::new(),
            };
            Some((value, style, conditions))
        } else {
            None
        };
        // A container's picture keeps its frame: it does not shrink.
        let container = match object.kind() == ObjectKind::Field && (text || shrinks) {
            true => content.container(object, context)?,
            false => None,
        };
        if let (Some((value, style, _)), true, None) = (&value, shrinks, &container) {
            let rect = &part.rects[i];
            shrink[i] = (rect.height() - prepare_text(value, rect, style)?.1).max(0.0);
        }
        values.push(value);
        data.push(container);
    }
    let mut rise = vec![0.0; count];
    for &o in &part.order {
        let Some(least) = part.nearest[o]
            .iter()
            .map(|&a| rise[a] + shrink[a])
            .reduce(f64::min)
        else {
            continue;
        };
        rise[o] = part.colliders[o]
            .iter()
            .map(|&c| part.rects[o].top - (part.rects[c].bottom - rise[c] - shrink[c]))
            .fold(least, f64::min)
            .max(0.0);
    }
    let printed = (height
        - part
            .resizers
            .iter()
            .map(|&r| rise[r] + shrink[r])
            .reduce(f64::min)
            .unwrap_or(0.0))
    .max(0.0);
    let mut laid = Vec::with_capacity(count);
    for (i, (value, container)) in values.into_iter().zip(data).enumerate() {
        // A portal's objects print in its rows, with it.
        if part.in_portal[i].is_some() {
            continue;
        }
        let rect = &part.rects[i];
        let (top, bottom) = (rect.top - rise[i], rect.bottom - rise[i] - shrink[i]);
        if bottom > printed + SLIDE_EPSILON {
            return Err(ReportError::unsupported(format!(
                "Report object {} would print below its resized part; \
                 set it to slide up and resize the part as well",
                part.objects[i].0.id
            )));
        }
        // An empty shrinking field collapses: no frame, fill or borders.
        if shrink[i] > 0.0 && bottom <= top {
            continue;
        }
        let bounds = Rect {
            top: top + origin.1,
            bottom: bottom + origin.1,
            left: rect.left + origin.0,
            right: rect.right + origin.0,
            ..Default::default()
        };
        let (lines, conditions, icon) = match value {
            Some((value, style, conditions)) if text => {
                let object = part.objects[i].0;
                let icon = icon_square(object, &bounds, &value);
                // A label beside an icon starts its side plus 10 points
                // from the frame's left edge, as in Browse.
                let label = match &icon {
                    Some(icon) if !value.is_empty() => Rect {
                        left: bounds.left + icon.width() + 10.0,
                        ..bounds.clone()
                    },
                    _ => bounds.clone(),
                };
                (
                    prepare_text_in(
                        &value,
                        &label,
                        &style,
                        overflow_of(object, part.slides[i].is_some()),
                    )?
                    .0,
                    conditions,
                    icon,
                )
            }
            _ => (Vec::new(), Vec::new(), None),
        };
        let placed = part.objects[i].0;
        laid.push(LaidOutObject {
            index: i,
            bounds,
            text: lines,
            conditions,
            icon,
            fill: None,
            container,
        });
        if placed.kind() == ObjectKind::Portal && text {
            let rect = laid.last().map(|o| o.bounds.clone()).unwrap_or_default();
            portal_rows(layout, part, i, &rect, context, content, &mut laid)?;
        }
    }
    // Browse draws a tab control's tabs after its front panel and what the
    // panel holds (which follow the control in stacking order).
    if text {
        for c in (0..count).filter(|&c| part.objects[c].0.kind() == ObjectKind::TabControl) {
            let Some(at) = laid.iter().position(|o| o.index == c && o.fill.is_none()) else {
                continue;
            };
            let rect = laid[at].bounds.clone();
            let control = part.objects[c].0;
            let mut end = c;
            while end + 1 < count && holds(layout, control, part.objects[end + 1].0) {
                end += 1;
            }
            let after = laid
                .iter()
                .rposition(|o| (c..=end).contains(&o.index))
                .map_or(at + 1, |p| p + 1);
            let front = front_panel(layout, control, content.front_panel(control));
            let mut tabs = Vec::new();
            tab_headers(layout, control, &rect, (c, front), &mut tabs)?;
            laid.splice(after..after, tabs);
        }
    }
    Ok(LaidOutPart {
        objects: laid,
        height: printed,
    })
}

/// Whether `container` holds `object`, at any depth.
fn holds(layout: &Layout, container: &LayoutObject, object: &LayoutObject) -> bool {
    let mut at = object.parent;
    // A chain longer than the store is a cycle.
    for _ in 0..=layout.objects.objects.len() {
        match at {
            Some(id) if id == container.id => return true,
            Some(id) => at = layout.objects.get(id).and_then(|o| o.parent),
            None => return false,
        }
    }
    false
}

/// Lay out portal `index`'s rows at `rect` (its page bounds), after it:
/// each row's background in the portal's row style (alternate rows in
/// theirs, as Browse paints them), and, where the row has a related
/// record, the portal's first-row objects moved into it, reading that
/// record. FileMaker prints the rows a portal shows without scrolling
/// (Claris, "Printing records"); empty rows print only their background.
fn portal_rows(
    layout: &Layout,
    part: &PartObjects<'_, '_>,
    index: usize,
    rect: &Rect,
    context: ContentContext,
    content: &mut impl ReportContent,
    laid: &mut Vec<LaidOutObject>,
) -> Result<(), ReportError> {
    let portal = part.objects[index].0;
    let refused = || {
        ReportError::unsupported(format!(
            "Report portal {}'s rows could not be laid out",
            portal.id
        ))
    };
    let geometry = portal_geometry(layout, portal, rect).ok_or_else(refused)?;
    let occurrence = (portal.portal.as_ref())
        .and_then(|p| om_layout::occurrence_id(p.table_alias_key))
        .ok_or_else(refused)?;
    let records = content.portal_records(portal, context)?.ok_or_else(|| {
        ReportError::unsupported(format!(
            "Report portal {}'s related records were not read",
            portal.id
        ))
    })?;
    // Browse: rows in component 12, alternate ones also in 13 when
    // property 610 is set; a portal row printed is never the active one.
    let normal = layout.object_style(portal, 12, 1);
    let alternate = layout.object_style(portal, 13, 1);
    let alternates = (layout.object_property(portal, 1, 1, 610))
        .and_then(|p| p.int)
        .is_some_and(|v| v != 0);
    let members: Vec<usize> = (0..part.objects.len())
        .filter(|&j| part.in_portal[j] == Some(index))
        .collect();
    // Each row's borders, drawn after every row and its objects, as Browse
    // draws them.
    let mut borders = Vec::new();
    for row in 0..geometry.rows {
        let at = geometry.first + row;
        let top = geometry.viewport.top + row as f64 * geometry.row_height;
        let mut style = normal.clone();
        if alternates && at % 2 == 1 {
            style.overlay(&alternate);
        }
        let bounds = Rect {
            top,
            bottom: (top + geometry.row_height).min(geometry.viewport.bottom),
            ..geometry.viewport.clone()
        };
        let mut fill = style.clone();
        fill.border_width = [None; 4];
        laid.push(LaidOutObject {
            index,
            bounds: bounds.clone(),
            text: Vec::new(),
            conditions: Vec::new(),
            icon: None,
            fill: Some((fill, 0.0)),
            container: None,
        });
        style.background = None;
        if style
            .border_width
            .iter()
            .any(|w| w.is_some_and(|w| w > 0.0))
        {
            borders.push((bounds, style));
        }
        let Some(&record) = records.get(at) else {
            continue;
        };
        let context = ContentContext {
            portal: Some(PortalRowContext {
                occurrence,
                record,
                number: at + 1,
            }),
            ..context
        };
        // First-row objects move by the frame's borders and the row.
        let (dx, dy) = (
            rect.left - part.rects[index].left + geometry.offset.0,
            rect.top - part.rects[index].top + geometry.offset.1 + row as f64 * geometry.row_height,
        );
        for &j in &members {
            let paged = on_page(part.objects[j].0, context.page);
            let object = paged.as_ref();
            let r = &part.rects[j];
            let bounds = Rect {
                left: r.left + dx,
                right: r.right + dx,
                top: r.top + dy,
                bottom: r.bottom + dy,
                ..Default::default()
            };
            // A row only partly in the frame: Browse clips what crosses its
            // bottom; nothing clips a report object to its portal, so such
            // an object is left out.
            if bounds.bottom > geometry.viewport.bottom + 0.01 {
                continue;
            }
            let (text, conditions, icon) = match object.kind() {
                ObjectKind::Text | ObjectKind::Field | ObjectKind::Button => {
                    let conditions = match object.calcs.conditions.is_empty() {
                        true => Vec::new(),
                        false => content.conditions(object, context)?.ok_or_else(|| {
                            ReportError::unsupported(format!(
                                "Report object {}'s conditional formatting was not evaluated",
                                object.id
                            ))
                        })?,
                    };
                    let style = text_box_style(layout, object, &conditions)?;
                    let value = match object.shows_label() {
                        true => content.text(object, context)?,
                        false => String::new(),
                    };
                    let icon = icon_square(object, &bounds, &value);
                    let label = match &icon {
                        Some(icon) if !value.is_empty() => Rect {
                            left: bounds.left + icon.width() + 10.0,
                            ..bounds.clone()
                        },
                        _ => bounds.clone(),
                    };
                    (
                        prepare_text_in(
                            &value,
                            &label,
                            &style,
                            overflow_of(object, part.slides[j].is_some()),
                        )?
                        .0,
                        conditions,
                        icon,
                    )
                }
                _ => (Vec::new(), Vec::new(), None),
            };
            // A container field in the row shows the row record's data.
            let container = match object.kind() == ObjectKind::Field {
                true => content.container(object, context)?,
                false => None,
            };
            laid.push(LaidOutObject {
                index: j,
                bounds,
                text,
                conditions,
                icon,
                fill: None,
                container,
            });
        }
    }
    for (bounds, style) in borders {
        laid.push(LaidOutObject {
            index,
            bounds,
            text: Vec::new(),
            conditions: Vec::new(),
            icon: None,
            fill: Some((style, 0.0)),
            container: None,
        });
    }
    Ok(())
}

/// A sub-summary part's break field, for its group summaries.
fn part_group(part: &Part) -> Result<Option<BreakField>, ReportError> {
    match part.kind() {
        PartKind::LeadingSubSummary | PartKind::TrailingSubSummary => {
            part.break_field().map_err(ReportError::unsupported)
        }
        _ => Ok(None),
    }
}

/// The shared page plan of Preview, PDF and Print. A report whose sliding
/// objects resize parts ([`resizes_parts`]) is measured: each such part is
/// laid out for its record on the page it lands on, reading only its
/// shrinking fields' content. Other reports plan from the record count.
pub(crate) fn plan_report(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    mut content: impl FnMut(&LayoutObject, ContentContext) -> Result<String, ReportError>,
) -> Result<ReportPlan, ReportError> {
    plan_report_with(layout, records, paper, breaks, &mut content)
}

/// [`plan_report`] with container data ([`ReportContent::container`]),
/// which a sliding container's measured height depends on.
pub(crate) fn plan_report_with(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    content: &mut impl ReportContent,
) -> Result<ReportPlan, ReportError> {
    if !resizes_parts(layout) {
        return paginate(layout, records, paper, breaks).map_err(ReportError::unsupported);
    }
    if records as u64 > MAX_GROUPED_RECORDS {
        return Err(ReportError::unsupported(format!(
            "Reports whose parts resize support at most {MAX_GROUPED_RECORDS} records"
        )));
    }
    let fronts = chosen_panels(layout, content);
    let objects = report_objects(layout, &paper, &fronts)?;
    let parts: Vec<_> = (layout.parts.iter().enumerate())
        .map(|(i, part)| part_objects(&objects, i, part))
        .collect();
    om_layout::pagination::paginate_with(layout, records, paper, breaks, |part, record, page| {
        let source = &layout.parts[part];
        if parts[part].resizers.is_empty() {
            return Ok(source.height);
        }
        let context = ContentContext {
            record,
            page,
            group: part_group(source)?,
            portal: None,
        };
        Ok(layout_part(
            layout,
            &parts[part],
            source.height,
            (0.0, 0.0),
            context,
            false,
            content,
        )?
        .height)
    })
}

impl From<String> for ReportError {
    fn from(message: String) -> Self {
        Self::unsupported(message)
    }
}

/// Prepare only the selected physical page. The callback receives both the
/// found-set record position and the actual physical page. Both backends
/// consume this scene, including root stacking, sliding, text wrapping and
/// clipping. Static pictures come from the layout's library through
/// `pictures`, never from record content.
pub(crate) fn prepare_page(
    layout: &Layout,
    plan: &ReportPlan,
    page: usize,
    pictures: &Pictures,
    content: &mut impl ReportContent,
) -> Result<ReportPage, ReportError> {
    let fronts = chosen_panels(layout, content);
    let objects = report_objects(layout, &plan.paper, &fronts)?;
    let placements = plan
        .pages
        .get(page)
        .ok_or_else(|| ReportError::unsupported("The report page is unavailable"))?;
    // The printable area, as the parts span it.
    let area = Rect {
        left: plan.paper.margins.left,
        top: plan.paper.margins.top,
        right: plan.paper.margins.left + plan.paper.printable_width(),
        bottom: plan.paper.height - plan.paper.margins.bottom,
        ..Default::default()
    };
    // FileMaker Pro first fills the printable area with the layout
    // background (style object type 20), then draws the parts over it
    // (layout-model.md 4.3, measured in its PDFs).
    let mut parts = Vec::new();
    parts.extend(fill_part(area.clone(), layout.background_style(1, 1)));
    // Each part's slide geometry once per page, not per placement.
    let mut part_cache: BTreeMap<usize, PartObjects<'_, '_>> = BTreeMap::new();
    for placement in placements {
        let part = &layout.parts[placement.part];
        let in_part = part_cache
            .entry(placement.part)
            .or_insert_with(|| part_objects(&objects, placement.part, part));
        let context = ContentContext {
            record: placement.record,
            page,
            group: part_group(part)?,
            portal: None,
        };
        let laid = layout_part(
            layout,
            in_part,
            part.height,
            (plan.paper.margins.left, placement.top),
            context,
            true,
            content,
        )?;
        // The plan placed this part at the height it measured then; content
        // that now lays out otherwise would overlap or open gaps.
        if (laid.height - placement.height).abs() > 0.001 {
            return Err(ReportError::unsupported(
                "The report content changed after its pages were planned",
            ));
        }
        if corner_radius(|id| layout.part_property(part, 1, 1, id), "A report part")? != 0.0 {
            return Err(ReportError::unsupported(
                "Rounded report parts are unsupported",
            ));
        }
        let mut prepared = ReportPart {
            bounds: Rect {
                left: plan.paper.margins.left,
                top: placement.top,
                right: plan.paper.margins.left + plan.paper.printable_width(),
                bottom: placement.top + placement.height,
                ..Default::default()
            },
            style: layout.part_style(part, 1, 1),
            objects: Vec::new(),
        };
        for laid in laid.objects {
            let LaidOutObject {
                index,
                bounds,
                mut text,
                conditions,
                icon,
                fill,
                container: data,
            } = laid;
            // A portal row's background, or a tab with its label.
            if let Some((style, radius)) = fill {
                prepared.objects.push(ReportObject {
                    kind: ObjectKind::Rectangle,
                    bounds,
                    style,
                    text,
                    picture: None,
                    placed: None,
                    file_icon: None,
                    radius,
                    icon: None,
                    line: None,
                });
                continue;
            }
            let object = in_part.objects[index].0;
            // Browse paints a tab panel, the one shown, in its active state.
            let state = if object.kind() == ObjectKind::TabPanel {
                7
            } else {
                1
            };
            // A condition's corner radii would round a frame `corner_radius`
            // read square.
            if conditions.iter().any(|&i| {
                (object.calcs.conditions.get(i))
                    .and_then(|format| format.condition.style.as_ref())
                    .and_then(|style| style.local_styles.as_ref())
                    .is_some_and(|styles| {
                        (styles.components.iter())
                            .any(|c| c.properties.iter().any(|p| CORNER_RADII.contains(&p.id)))
                    })
            }) {
                return Err(ReportError::unsupported(format!(
                    "Report object {} has conditional corner radii; they are unsupported",
                    object.id
                )));
            }
            let style = crate::document::formatted_style(layout, object, &conditions, 1, state);
            let mut picture = if object.kind() == ObjectKind::Graphic {
                Some(pictures.get(layout, object)?)
            } else {
                None
            };
            let (mut placed, mut file_icon) = (None, None);
            let frame = match &data {
                Some(_) => Some(content_box(&bounds, &style, layout, object, &conditions)?),
                None => None,
            };
            // Borders and padding that leave no room print the field empty,
            // as Browse draws nothing there.
            let room = |frame: &Rect| frame.width() > 0.0 && frame.height() > 0.0;
            if let (Some(data), Some(frame)) = (&data, frame.filter(room)) {
                let format = object.graphic_format.unwrap_or_default();
                match pictures.container(&data.data, format, (frame.width(), frame.height())) {
                    Some(Some((shown, [x, y, w, h]))) => {
                        picture = Some(shown);
                        let image = Rect {
                            left: frame.left + x,
                            top: frame.top + y,
                            right: frame.left + x + w,
                            bottom: frame.top + y + h,
                            ..Default::default()
                        };
                        placed = Some([frame, image]);
                    }
                    // Data that does not decode prints the field empty.
                    Some(None) => {}
                    None => {
                        (file_icon, text) = file(layout, object, &conditions, &frame, &data.data)?
                    }
                }
            }
            let radius = match object.kind() {
                ObjectKind::Line | ObjectKind::Oval => 0.0,
                kind => {
                    let radius = corner_radius(
                        |id| layout.object_property(object, 1, state, id),
                        &format!("Report object {}", object.id),
                    )?;
                    if kind == ObjectKind::Graphic && radius != 0.0 {
                        return Err(ReportError::unsupported(format!(
                            "Report picture {} has a rounded frame; rounded picture frames are \
                             unsupported",
                            object.id
                        )));
                    }
                    radius
                }
            };
            // As Browse tints it: the icon component's color, else the
            // frame's top border color (conditions apply to the frame only).
            let icon = match (icon, &object.button) {
                (Some(square), Some(button)) => {
                    let component = crate::document::formatted_style(layout, object, &[], 4, 1);
                    let tint = (component.icon_color)
                        .or(component.color)
                        .or(style.border_color[0]);
                    let side = (square.width() * ICON_SCALE).ceil().clamp(1.0, 1024.0) as u32;
                    let picture = pictures.icon(layout, object, button.library_key, tint, side)?;
                    Some((picture, square))
                }
                _ => None,
            };
            let line =
                (object.kind() == ObjectKind::Line).then(|| line_endpoints(&bounds, object.flags));
            let caption = is_web_viewer(object)
                .then(|| web_viewer_caption(object, &bounds, &plan.paper))
                .transpose()?;
            // Layout Setup's "Delineate fields on current record only"
            // (layout option bit 0): a field's borders and fill show on the
            // current record only, and not when printing, saving as PDF or
            // previewing (Claris, "Showing field borders and fill for the
            // current record"; FileMaker Pro 26, #263, #536). Its box keeps
            // the borders' inset, and a container keeps its picture.
            let mut style = style;
            if object.kind() == ObjectKind::Field && delineates_current_record_only(layout) {
                style.border_width = [None; 4];
                style.background = None;
            }
            prepared.objects.push(ReportObject {
                kind: object.kind(),
                bounds,
                style,
                text,
                picture,
                placed,
                file_icon,
                radius,
                icon,
                line,
            });
            prepared.objects.extend(caption);
        }
        parts.push(prepared);
    }
    // Below the last part, down to the footer or the bottom of the
    // printable area, FileMaker fills the page with the body's fill (not
    // its borders): gray under a Details layout's body, white under a
    // list's last row.
    let footer = |p: &&om_layout::pagination::Placement| {
        matches!(
            layout.parts[p.part].kind(),
            PartKind::Footer | PartKind::TitleFooter
        )
    };
    let end = (placements.iter().filter(|p| !footer(p)))
        .map(|p| p.top + p.height)
        .fold(area.top, f64::max);
    let limit = (placements.iter().filter(footer))
        .map(|p| p.top)
        .fold(area.bottom, f64::min);
    if let Some(body) = layout.parts.iter().find(|p| p.kind() == PartKind::Body) {
        if limit > end {
            parts.extend(fill_part(
                Rect {
                    top: end,
                    bottom: limit,
                    ..area.clone()
                },
                layout.part_style(body, 1, 1),
            ));
        }
    }
    let scene = ReportPage { parts };
    // Encoding validates the identical shape/style refusal contract before
    // either backend displays a page. This does no I/O or font embedding.
    encode_page(
        &scene,
        plan.paper.height,
        &mut Default::default(),
        &mut Vec::new(),
        &mut BTreeSet::new(),
    )?;
    Ok(scene)
}

/// A part with no objects that only fills `bounds` with `style`'s fill,
/// without its borders; `None` when the style has no fill.
fn fill_part(bounds: Rect, style: ResolvedStyle) -> Option<ReportPart> {
    let background = style.background?;
    Some(ReportPart {
        bounds,
        style: ResolvedStyle {
            background: Some(background),
            ..Default::default()
        },
        objects: Vec::new(),
    })
}

/// `picture` stretched over `r`: the image's unit square, top row first.
fn draw_picture(
    c: &mut Content,
    r: &Rect,
    h: f64,
    images: &mut Vec<Arc<Picture>>,
    picture: &Arc<Picture>,
) {
    let name = image_name(image_index(images, picture));
    if r.width() > 0.0 && r.height() > 0.0 {
        c.save_state()
            .transform([
                r.width() as f32,
                0.0,
                0.0,
                r.height() as f32,
                r.left as f32,
                (h - r.bottom) as f32,
            ])
            .x_object(Name(name.as_bytes()))
            .restore_state();
    }
}

/// A container field's content box, as Browse places it: inside each
/// side's border width, drawn or not (`style`, its box), and the padding of
/// its text style.
fn content_box(
    bounds: &Rect,
    style: &ResolvedStyle,
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
) -> Result<Rect, ReportError> {
    let padding = text_style(layout, object, conditions)?.padding;
    let side = |i: usize| {
        crate::document::container_border(style, i) + padding[i].unwrap_or(0.0).clamp(0.0, 1000.0)
    };
    Ok(Rect {
        left: bounds.left + side(3),
        top: bounds.top + side(0),
        right: (bounds.right - side(1)).max(bounds.left + side(3)),
        bottom: (bounds.bottom - side(2)).max(bounds.top + side(0)),
        ..Default::default()
    })
}

/// A container file as Browse shows it: a 32-point document icon (less in
/// a small frame) centered above its name, in the field's text style.
fn file(
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
    frame: &Rect,
    data: &om_data::calc::Container,
) -> Result<(Option<Rect>, Vec<TextLine>), ReportError> {
    let mut style = text_style(layout, object, conditions)?;
    style.text_align = Some(2);
    style.vertical_align = Some(0);
    style.padding = [None; 4];
    let name = data.text.as_str();
    let line = if name.is_empty() {
        0.0
    } else {
        style.font_size.unwrap_or(12.0) * 1.3
    };
    let icon = 32f64.min(frame.height() - line - 4.0).min(frame.width());
    let top = frame.top + ((frame.height() - icon.max(0.0) - 2.0 - line) / 2.0).max(0.0);
    let center = (frame.left + frame.right) / 2.0;
    let shown = (icon >= 8.0).then(|| Rect {
        left: center - icon / 2.0,
        top,
        right: center + icon / 2.0,
        bottom: top + icon,
        ..Default::default()
    });
    let below = Rect {
        top: shown.as_ref().map_or(top, |icon| icon.bottom + 2.0),
        ..frame.clone()
    };
    let text = match below.height() > 0.0 {
        true => prepare_text(name, &below, &style)?.0,
        false => Vec::new(),
    };
    Ok((shown, text))
}

/// The document glyph of a container file, on the 20-point grid Browse's
/// icon uses (`icons::Icon::File`): an outline with a folded corner.
const FILE_GLYPH: [&[(f64, f64)]; 2] = [
    &[
        (5.0, 2.5),
        (12.0, 2.5),
        (15.5, 6.0),
        (15.5, 17.5),
        (5.0, 17.5),
        (5.0, 2.5),
    ],
    &[(12.0, 2.5), (12.0, 6.0), (15.5, 6.0)],
];

/// A picture's resource index in the document: each distinct picture is
/// embedded once, however many objects and pages show it.
fn image_index(images: &mut Vec<Arc<Picture>>, picture: &Arc<Picture>) -> usize {
    images
        .iter()
        .position(|p| Arc::ptr_eq(p, picture))
        .unwrap_or_else(|| {
            images.push(picture.clone());
            images.len() - 1
        })
}
fn image_name(index: usize) -> String {
    format!("Im{index}")
}

/// The page's content stream. The opacities its translucent colors use are
/// added to `alphas`, for the page's resources ([`alpha_name`]).
fn encode_page(
    scene: &ReportPage,
    h: f64,
    maps: &mut [BTreeMap<u16, char>; ReportFace::ALL.len()],
    images: &mut Vec<Arc<Picture>>,
    alphas: &mut BTreeSet<u8>,
) -> Result<Vec<u8>, ReportError> {
    let mut c = Content::new();
    for part in &scene.parts {
        c.save_state();
        pdf_rect(&mut c, &part.bounds, h);
        c.clip_nonzero().end_path();
        box_style(&mut c, &part.bounds, h, &part.style, 0.0, alphas)?;
        for object in &part.objects {
            let r = &object.bounds;
            let style = &object.style;
            match object.kind {
                ObjectKind::Line => {
                    let width = style.border_width[0].unwrap_or(1.0);
                    if !width.is_finite()
                        || width < 0.0
                        || !matches!(style.border_style[0], None | Some(5))
                    {
                        return Err(ReportError::unsupported("Unsupported report line style"));
                    }
                    let color = style.border_color[0].or(style.color).unwrap_or(Color {
                        alpha: 1.0,
                        ..Default::default()
                    });
                    let [(x0, y0), (x1, y1)] = object.line.unwrap_or_else(|| line_endpoints(r, 0));
                    if let Some(paint) = paint(color)? {
                        let opened = open(&mut c, &paint, alphas);
                        c.set_stroke_rgb(paint.rgb[0], paint.rgb[1], paint.rgb[2])
                            .set_line_width(width as f32)
                            .move_to(x0 as f32, (h - y0) as f32)
                            .line_to(x1 as f32, (h - y1) as f32)
                            .stroke();
                        close(&mut c, opened);
                    }
                }
                ObjectKind::Oval => {
                    if style.border_width.iter().any(Option::is_some)
                        || style.border_color.iter().any(Option::is_some)
                        || style.border_style.iter().any(Option::is_some)
                    {
                        return Err(ReportError::unsupported(
                            "Styled oval borders are unsupported",
                        ));
                    }
                    let (x, y, rx, ry) = (
                        ((r.left + r.right) / 2.0) as f32,
                        (h - (r.top + r.bottom) / 2.0) as f32,
                        (r.width() / 2.0) as f32,
                        (r.height() / 2.0) as f32,
                    );
                    let k = 0.552_284_8;
                    // The color (and a translucent one's state) before the path.
                    let filled = fill(&mut c, style.background, alphas)?;
                    if filled.is_none() {
                        c.set_stroke_rgb(0.0, 0.0, 0.0).set_line_width(1.0);
                    }
                    c.move_to(x + rx, y)
                        .cubic_to(x + rx, y + k * ry, x + k * rx, y + ry, x, y + ry)
                        .cubic_to(x - k * rx, y + ry, x - rx, y + k * ry, x - rx, y)
                        .cubic_to(x - rx, y - k * ry, x - k * rx, y - ry, x, y - ry)
                        .cubic_to(x + k * rx, y - ry, x + rx, y - k * ry, x + rx, y)
                        .close_path();
                    match filled {
                        Some(opened) => {
                            c.fill_nonzero();
                            close(&mut c, opened);
                        }
                        None => {
                            c.stroke();
                        }
                    }
                }
                ObjectKind::Graphic => {
                    box_style(&mut c, r, h, style, object.radius, alphas)?;
                    let picture = object.picture.as_ref().ok_or_else(|| {
                        ReportError::unsupported("A report picture was not prepared")
                    })?;
                    draw_picture(&mut c, r, h, images, picture);
                }
                _ => {
                    box_style(&mut c, r, h, style, object.radius, alphas)?;
                    if let (Some(picture), Some([clip, image])) = (&object.picture, &object.placed)
                    {
                        let name = image_name(image_index(images, picture));
                        c.save_state();
                        pdf_rect(&mut c, clip, h);
                        c.clip_nonzero().end_path();
                        if image.width() > 0.0 && image.height() > 0.0 {
                            c.transform([
                                image.width() as f32,
                                0.0,
                                0.0,
                                image.height() as f32,
                                image.left as f32,
                                (h - image.bottom) as f32,
                            ])
                            .x_object(Name(name.as_bytes()));
                        }
                        c.restore_state();
                    }
                    if let Some(icon) = &object.file_icon {
                        let color = style.color.unwrap_or(Color {
                            alpha: 1.0,
                            ..Default::default()
                        });
                        if let Some(paint) = paint(color)? {
                            let opened = open(&mut c, &paint, alphas);
                            let unit = icon.width() / 20.0;
                            let at = |(x, y): (f64, f64)| {
                                (
                                    (icon.left + x * unit) as f32,
                                    (h - icon.top - y * unit) as f32,
                                )
                            };
                            c.set_stroke_rgb(paint.rgb[0], paint.rgb[1], paint.rgb[2])
                                .set_line_width((unit * 1.3) as f32);
                            for path in FILE_GLYPH {
                                let (x, y) = at(path[0]);
                                c.move_to(x, y);
                                for &point in &path[1..] {
                                    let (x, y) = at(point);
                                    c.line_to(x, y);
                                }
                                c.stroke();
                            }
                            close(&mut c, opened);
                        }
                    }
                    if let Some((icon, square)) = &object.icon {
                        draw_picture(&mut c, square, h, images, icon);
                    }
                    for line in &object.text {
                        // An empty line (a blank paragraph) draws nothing,
                        // and must not select a face the PDF embeds only for
                        // the glyphs it uses.
                        if line.glyphs.is_empty() {
                            continue;
                        }
                        let bytes: Vec<u8> = line
                            .glyphs
                            .iter()
                            .flat_map(|(glyph, _, ch)| {
                                maps[line.face.index()].entry(*glyph).or_insert(*ch);
                                glyph.to_be_bytes()
                            })
                            .collect();
                        if let Some(clip) = &line.clip {
                            c.save_state();
                            pdf_rect(&mut c, clip, h);
                            c.clip_nonzero().end_path();
                        }
                        let Some(opened) = fill(&mut c, Some(line.color), alphas)? else {
                            if line.clip.is_some() {
                                c.restore_state();
                            }
                            continue;
                        };
                        c.begin_text()
                            .set_font(Name(line.face.names().0), line.size as f32)
                            .set_text_matrix([
                                1.0,
                                0.0,
                                0.0,
                                1.0,
                                line.x as f32,
                                (h - line.baseline) as f32,
                            ])
                            .show(Str(&bytes))
                            .end_text();
                        close(&mut c, opened);
                        if line.clip.is_some() {
                            c.restore_state();
                        }
                    }
                }
            }
        }
        c.restore_state();
    }
    Ok(c.finish().to_vec())
}

/// `content` reads an object's formatted text for a selected record and page.
/// The source may calculate fields, related values and summaries; a missing or
/// pending value must return an error, never an empty placeholder.
pub fn render_pdf(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    mut content: impl FnMut(&LayoutObject, ContentContext) -> Result<String, ReportError>,
) -> Result<PdfReport, ReportError> {
    render_pdf_with(layout, records, paper, breaks, &mut content)
}

/// [`render_pdf`] with container data ([`ReportContent::container`]) and
/// conditional formatting ([`ReportContent::conditions`]).
pub fn render_pdf_with(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    content: &mut impl ReportContent,
) -> Result<PdfReport, ReportError> {
    render_pdf_viewed(layout, records, paper, breaks, content, PdfView::default())
}

/// [`render_pdf_with`], opening on `view`.
pub fn render_pdf_viewed(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    content: &mut impl ReportContent,
    view: PdfView,
) -> Result<PdfReport, ReportError> {
    let plan = plan_report_with(layout, records, paper, breaks, content)?;
    let mut pdf = Pdf::new();
    let root = Ref::new(1);
    let pages = Ref::new(2);
    let mut catalog = pdf.catalog(root);
    catalog.pages(pages);
    match view {
        PdfView::PageOnly => {}
        PdfView::Bookmarks => {
            catalog.page_mode(PageMode::UseOutlines);
        }
        PdfView::Pages => {
            catalog.page_mode(PageMode::UseThumbs);
        }
    }
    catalog.finish();
    let mut maps: [BTreeMap<u16, char>; ReportFace::ALL.len()] = Default::default();
    // Every page decodes and embeds a library picture once, by document.
    let pictures = Pictures::default();
    let mut images = Vec::new();
    let image_ref = |index: usize| Ref::new(20 + (plan.pages.len() + index) as i32 * 2);
    let h = plan.paper.height;
    // Each page's pictures and opacities; the page dictionaries follow the
    // streams, once every picture (and so the first free object) is known.
    let mut resources = Vec::with_capacity(plan.pages.len());
    for index in 0..plan.pages.len() {
        let stream = Ref::new(21 + index as i32 * 2);
        let scene = prepare_page(layout, &plan, index, &pictures, content)?;
        let mut alphas = BTreeSet::new();
        pdf.stream(
            stream,
            &encode_page(&scene, h, &mut maps, &mut images, &mut alphas)?,
        );
        let used: BTreeSet<usize> = scene
            .parts
            .iter()
            .flat_map(|part| &part.objects)
            .flat_map(|object| {
                (object.picture.iter()).chain(object.icon.iter().map(|(icon, _)| icon))
            })
            .map(|picture| image_index(&mut images, picture))
            .collect();
        resources.push((used, alphas));
    }
    // One graphics state object per opacity the document uses, after the
    // pictures' objects (each picture takes two).
    let opacities: BTreeSet<u8> = resources
        .iter()
        .flat_map(|(_, a)| a.iter().copied())
        .collect();
    let first_free = 20 + (plan.pages.len() + images.len()) as i32 * 2;
    let alpha_ref = |alpha: u8| {
        Ref::new(first_free + opacities.iter().position(|&a| a == alpha).unwrap_or(0) as i32)
    };
    // Inter keeps objects 3-8 and 9-14; a Liberation Sans face the document
    // uses takes six objects after the opacity states.
    let after_alphas = first_free + opacities.len() as i32;
    let font_ref = |face: ReportFace| match face {
        ReportFace::Inter => 3,
        ReportFace::InterBold => 9,
        ReportFace::Sans => after_alphas,
        ReportFace::SansBold => after_alphas + 6,
        ReportFace::SansItalic => after_alphas + 12,
        ReportFace::SansBoldItalic => after_alphas + 18,
    };
    let fonts_used: Vec<ReportFace> = ReportFace::ALL
        .into_iter()
        .filter(|face| !face.is_sans() || !maps[face.index()].is_empty())
        .collect();
    for (index, (used, alphas)) in resources.into_iter().enumerate() {
        let page_id = Ref::new(20 + index as i32 * 2);
        let stream = Ref::new(21 + index as i32 * 2);
        let mut page = pdf.page(page_id);
        page.parent(pages)
            .media_box(pdf_writer::Rect::new(
                0.0,
                0.0,
                plan.paper.width as f32,
                h as f32,
            ))
            .contents(stream);
        let mut resources = page.resources();
        let mut fonts = resources.fonts();
        for &face in &fonts_used {
            fonts.pair(Name(face.names().0), Ref::new(font_ref(face)));
        }
        fonts.finish();
        if !used.is_empty() {
            let mut x_objects = resources.x_objects();
            for index in used {
                x_objects.pair(Name(image_name(index).as_bytes()), image_ref(index));
            }
        }
        if !alphas.is_empty() {
            let mut states = resources.ext_g_states();
            for alpha in alphas {
                states.pair(Name(alpha_name(alpha).as_bytes()), alpha_ref(alpha));
            }
        }
    }
    for &alpha in &opacities {
        let opacity = f32::from(alpha) / 255.0;
        pdf.ext_graphics(alpha_ref(alpha))
            .non_stroking_alpha(opacity)
            .stroking_alpha(opacity);
    }
    pdf.pages(pages)
        .kids((0..plan.pages.len()).map(|i| Ref::new(20 + i as i32 * 2)))
        .count(plan.pages.len() as i32);
    for &face in &fonts_used {
        embed(&mut pdf, face, font_ref(face), &maps[face.index()]);
    }
    for (index, picture) in images.iter().enumerate() {
        embed_picture(&mut pdf, image_ref(index), picture)?;
    }
    Ok(PdfReport {
        plan,
        bytes: pdf.finish(),
    })
}

fn deflate(samples: &[u8]) -> Result<Vec<u8>, ReportError> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(samples)
        .and_then(|()| encoder.finish())
        .map_err(|e| {
            ReportError::unsupported(format!("A report picture could not be encoded: {e}"))
        })
}

/// An 8-bit DeviceRGB image. Transparency becomes a DeviceGray soft mask over
/// the straight color samples, so nothing is premultiplied twice or painted
/// over a white box. The mask, when present, is the next object number.
fn embed_picture(pdf: &mut Pdf, id: Ref, picture: &Picture) -> Result<(), ReportError> {
    let (pixels, _) = picture.rgba.as_chunks::<4>();
    let color = deflate(
        &pixels
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect::<Vec<_>>(),
    )?;
    let mask = if picture.opaque {
        None
    } else {
        Some(deflate(&pixels.iter().map(|p| p[3]).collect::<Vec<_>>())?)
    };
    let (width, height) = (picture.width as i32, picture.height as i32);
    let mut image = pdf.image_xobject(id, &color);
    image.filter(Filter::FlateDecode);
    image.width(width).height(height).bits_per_component(8);
    image.color_space().device_rgb();
    if mask.is_some() {
        image.s_mask(Ref::new(id.get() + 1));
    }
    image.finish();
    if let Some(mask) = mask {
        let mut alpha = pdf.image_xobject(Ref::new(id.get() + 1), &mask);
        alpha.filter(Filter::FlateDecode);
        alpha.width(width).height(height).bits_per_component(8);
        alpha.color_space().device_gray();
    }
    Ok(())
}

/// Stage beside the destination and atomically replace only a complete PDF.
/// Cancellation/validation happen before this function. Failed writes leave the
/// previous destination intact and tempfile removes the staging file.
pub fn save_pdf(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_file(path, bytes)
}

/// Write `bytes` to `path` as [`save_pdf`] does: staged beside it, then
/// renamed over it only once complete (Export Field Contents too). The
/// file gets a new file's permissions (the user's umask applies, not a
/// temporary file's owner-only ones); a file it replaces keeps its own,
/// and until the staged file has them it stays owner-only, so a private
/// file's new contents are never readable by others on the way.
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let existing = std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.permissions());
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // A replacement keeps tempfile's owner-only default until it is
        // given the old file's permissions.
        if existing.is_none() {
            builder.permissions(std::fs::Permissions::from_mode(0o666));
        }
    }
    let mut temp = builder.tempfile_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if let Some(permissions) = existing {
        temp.as_file().set_permissions(permissions)?;
    }
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// The definition of a field a report shows, or why the report refuses it
/// before any value is read.
pub(crate) fn field_def<'s>(
    schema: &'s om_data::host::CalcSchema,
    field: om_data::runtime::FieldRef,
    object: &LayoutObject,
) -> Result<&'s om_data::host::FieldDef, ReportError> {
    let def = schema
        .field(field.occurrence, field.field)
        .ok_or_else(|| ReportError {
            code: 102,
            message: "A report field is missing".into(),
        })?;
    // A container field object prints its data
    // ([`ReportContent::container`]); a merge field has no text for it.
    if def.value_type == om_data::calc::ValueType::Container && object.kind() != ObjectKind::Field {
        return Err(ReportError::unsupported(
            "Container merge fields in reports are unsupported",
        ));
    }
    Ok(def)
}

/// Resolve the shared report field/merge binding contract. The caller supplies
/// read-only per-record calculation and display formatting.
pub(crate) fn object_text(
    object: &LayoutObject,
    origin: u64,
    schema: &om_data::host::CalcSchema,
    mut field_text: impl FnMut(om_data::runtime::FieldRef) -> Result<String, ReportError>,
) -> Result<String, ReportError> {
    use om_data::formula::Names;
    use om_data::runtime::FieldRef;
    use om_layout::TextRun;
    if object.kind() == ObjectKind::Field {
        let field = object
            .field_obj
            .as_ref()
            .ok_or_else(|| ReportError::unsupported("A report field has no binding"))?;
        if field.num_of_reps > 1 || matches!(field.display_type, 3..=5) {
            return Err(ReportError::unsupported(
                "Repeating fields, option sets and container controls in reports are unsupported",
            ));
        }
        let spec = field
            .field_spec
            .as_ref()
            .ok_or_else(|| ReportError::unsupported("A report field has no binding"))?;
        let occurrence = om_layout::occurrence_id(spec.table_key)
            .ok_or_else(|| ReportError::unsupported("A report field occurrence is invalid"))?;
        let id = spec
            .field_id()
            .ok_or_else(|| ReportError::unsupported("A report field key is invalid"))?;
        return field_text(FieldRef {
            occurrence,
            field: id,
            repetition: spec.repetition.max(1) as u32,
        });
    }
    if object.text_runs.is_empty() {
        return Ok(object.text.clone().unwrap_or_default());
    }
    let mut text = String::new();
    for run in &object.text_runs {
        match run {
            TextRun::Literal(literal) => text.push_str(literal),
            TextRun::MergeField(name) => {
                let (occurrence, name) = match name.split_once("::") {
                    Some((to, name)) => (
                        schema.names().find_occurrence(to).ok_or_else(|| {
                            ReportError::unsupported("A report merge occurrence is missing")
                        })?,
                        name,
                    ),
                    None => (origin, name.as_str()),
                };
                let field = schema
                    .names()
                    .find_field(occurrence, name)
                    .ok_or_else(|| ReportError::unsupported("A report merge field is missing"))?;
                text.push_str(&field_text(FieldRef {
                    occurrence,
                    field,
                    repetition: 1,
                })?);
            }
        }
    }
    Ok(text)
}

#[cfg(test)]
pub(crate) mod tests {
    //! Anonymous synthetic pictures, generated here, and the pixels both
    //! backends must show for them.
    use super::*;
    use om_layout::pagination::Placement;
    use om_layout::{GraphicObject, ObjectStyle, Property, StyleSet};

    const GREEN: Color = Color {
        red: 0.1,
        green: 0.6,
        blue: 0.2,
        alpha: 1.0,
    };
    const ORANGE: Color = Color {
        red: 1.0,
        green: 0.5,
        blue: 0.0,
        alpha: 1.0,
    };
    pub(crate) const PNG_KEY: u64 = 7;
    pub(crate) const JPEG_KEY: u64 = 8;

    fn encode(image: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    /// 40 x 10: transparent magenta, red over blue, half-transparent white,
    /// black. Any lost or doubled alpha, or a flip, changes a sampled color.
    pub(crate) fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_fn(40, 10, |x, y| {
            image::Rgba(match x / 10 {
                0 => [255, 0, 255, 0],
                1 if y < 5 => [255, 0, 0, 255],
                1 => [0, 0, 255, 255],
                2 => [255, 255, 255, 128],
                _ => [0, 0, 0, 255],
            })
        });
        encode(image.into(), image::ImageFormat::Png)
    }

    /// 64 x 16 quadrants: red over blue on the left, yellow over black on the
    /// right.
    pub(crate) fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_fn(64, 16, |x, y| {
            image::Rgb(match (x < 32, y < 8) {
                (true, true) => [220, 30, 30],
                (true, false) => [30, 30, 220],
                (false, true) => [230, 210, 40],
                (false, false) => [20, 20, 20],
            })
        });
        encode(image.into(), image::ImageFormat::Jpeg)
    }

    /// Neutral static-format vectors: opaque red above blue, and an SVG
    /// with its own colors, alpha and bundled-font text (never an icon tint).
    pub(crate) fn extra_pictures() -> Vec<(u64, LibraryImageFormat, Vec<u8>)> {
        use image::ImageFormat as F;
        let mut result = Vec::new();
        for (index, format, encoded) in [
            (0, LibraryImageFormat::Gif, F::Gif),
            (1, LibraryImageFormat::Bmp, F::Bmp),
            (2, LibraryImageFormat::Tiff, F::Tiff),
            (3, LibraryImageFormat::WebP, F::WebP),
        ] {
            let pixels = image::RgbaImage::from_fn(28, 10, |_, y| {
                image::Rgba(if y < 5 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                })
            });
            result.push((100 + index, format, encode(pixels.into(), encoded)));
        }
        result.push((104, LibraryImageFormat::Svg, br#"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="10"><rect width="28" height="5" fill="red"/><rect y="5" width="28" height="5" fill="blue"/><rect width="2" height="10" fill="white" opacity="0.5"/><text x="2" y="8" font-family="Inter" font-size="6" fill="white">A</text></svg>"#.to_vec()));
        result
    }

    fn filled(color: Color) -> Option<ObjectStyle> {
        Some(ObjectStyle {
            local_styles: Some(StyleSet {
                components: vec![om_layout::style::ComponentStyle {
                    component: 1,
                    state: 1,
                    properties: vec![Property {
                        id: 7,
                        color: Some(color),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        })
    }
    fn at(left: f64, top: f64, width: f64, height: f64) -> Option<Rect> {
        Some(Rect {
            left,
            top,
            right: left + width,
            bottom: top + height,
            ..Default::default()
        })
    }
    fn picture(id: u64, key: u64, bounds: Option<Rect>) -> LayoutObject {
        LayoutObject {
            id,
            object_type: 3,
            bounds,
            graphic: Some(GraphicObject {
                library_key: key,
                // Not interpreted: the bytes' own signature decides.
                pict_format: u32::from_be_bytes(*b"PNGf"),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Add pictures to a layout with the PDF fixture's header (0-80), body
    /// (80-220) and footer (220-260). The header stacks a green frame, the
    /// PNG, a label and an orange patch; storage order is the reverse of that
    /// root order. Each body repeats the JPEG; the footer repeats the PNG.
    pub(crate) fn add_pictures(layout: &mut Layout) {
        let label = LayoutObject {
            id: 42,
            object_type: 2,
            text: Some("Logo".into()),
            bounds: at(402.0, 18.0, 36.0, 16.0),
            ..Default::default()
        };
        let mut added = vec![
            LayoutObject {
                id: 40,
                object_type: 5,
                bounds: at(300.0, 8.0, 220.0, 64.0),
                style: filled(GREEN),
                ..Default::default()
            },
            picture(41, PNG_KEY, at(320.0, 16.0, 160.0, 48.0)),
            label,
            LayoutObject {
                id: 43,
                object_type: 5,
                bounds: at(450.0, 44.0, 20.0, 16.0),
                style: filled(ORANGE),
                ..Default::default()
            },
            picture(44, JPEG_KEY, at(360.0, 172.0, 120.0, 36.0)),
            picture(45, PNG_KEY, at(320.0, 228.0, 72.0, 18.0)),
        ];
        for (index, (key, format, data)) in extra_pictures().into_iter().enumerate() {
            added.push(picture(
                500 + index as u64,
                key,
                at(320.0 + index as f64 * 36.0, 66.0, 28.0, 10.0),
            ));
            layout
                .library_images
                .insert(key, LibraryImage { format, data });
        }
        layout.objects.root.extend(added.iter().map(|o| o.id));
        for object in added {
            layout.objects.objects.insert(0, object);
        }
        for (key, format, data) in [
            (PNG_KEY, LibraryImageFormat::Png, png()),
            (JPEG_KEY, LibraryImageFormat::Jpeg, jpeg()),
        ] {
            layout
                .library_images
                .insert(key, LibraryImage { format, data });
        }
    }

    /// Part-relative layout points and the color each backend must show.
    fn samples(kind: PartKind) -> &'static [(&'static str, f64, f64, [u8; 3], u8)] {
        match kind {
            PartKind::Header => &[
                ("frame", 310.0, 40.0, [25, 153, 51], 14),
                ("transparent PNG band", 340.0, 30.0, [25, 153, 51], 14),
                ("PNG top row (red)", 380.0, 24.0, [255, 0, 0], 14),
                ("PNG bottom row (blue)", 380.0, 56.0, [0, 0, 255], 14),
                ("half-transparent white", 420.0, 50.0, [141, 204, 153], 16),
                ("opaque black", 460.0, 24.0, [0, 0, 0], 14),
                ("patch stacked above PNG", 460.0, 52.0, [255, 127, 0], 14),
                ("GIF red", 340.0, 68.0, [255, 0, 0], 14),
                ("GIF blue", 340.0, 74.0, [0, 0, 255], 14),
                ("BMP red", 376.0, 68.0, [255, 0, 0], 14),
                ("BMP blue", 376.0, 74.0, [0, 0, 255], 14),
                ("TIFF red", 412.0, 68.0, [255, 0, 0], 14),
                ("TIFF blue", 412.0, 74.0, [0, 0, 255], 14),
                ("WebP red", 448.0, 68.0, [255, 0, 0], 14),
                ("WebP blue", 448.0, 74.0, [0, 0, 255], 14),
                ("SVG red", 484.0, 68.0, [255, 0, 0], 14),
                ("SVG blue", 484.0, 74.0, [0, 0, 255], 14),
            ],
            PartKind::Body => &[
                ("JPEG top left", 390.9, 102.1, [220, 30, 30], 40),
                ("JPEG bottom left", 390.9, 120.1, [30, 30, 220], 40),
                ("JPEG top right", 450.9, 102.1, [230, 210, 40], 40),
                ("JPEG bottom right", 450.9, 120.1, [20, 20, 20], 40),
            ],
            PartKind::Footer => &[
                ("footer transparent PNG", 329.0, 12.5, [255, 255, 255], 14),
                ("footer PNG red", 347.9, 12.5, [255, 0, 0], 14),
                ("footer PNG blue", 347.9, 21.5, [0, 0, 255], 14),
                ("footer PNG black", 383.9, 12.5, [0, 0, 0], 14),
            ],
            _ => &[],
        }
    }

    /// Check one physical page through `pixel`, which reads the color at a
    /// page point. Returns the number of parts sampled.
    pub(crate) fn assert_pictures(
        layout: &Layout,
        plan: &ReportPlan,
        page: usize,
        what: &str,
        pixel: impl Fn(f64, f64) -> [u8; 3],
    ) -> usize {
        let mut sampled = 0;
        for &Placement { part, top, .. } in &plan.pages[page] {
            let kind = layout.parts[part].kind();
            for &(name, x, y, expected, tolerance) in samples(kind) {
                let (x, y) = (plan.paper.margins.left + x, top + y);
                let actual = pixel(x, y);
                assert!(
                    actual
                        .iter()
                        .zip(expected)
                        .all(|(a, e)| a.abs_diff(e) <= tolerance),
                    "{what} page {}: {name} at ({x}, {y}) is {actual:?}, expected {expected:?}",
                    page + 1
                );
            }
            sampled += usize::from(!samples(kind).is_empty());
        }
        sampled
    }

    /// The same image with an EXIF orientation 6 (rotate 90 degrees clockwise)
    /// APP1 segment, which Browse's loader applies.
    fn rotated_jpeg() -> Vec<u8> {
        let jpeg = jpeg();
        let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01".to_vec();
        exif.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
        let mut rotated = jpeg[..2].to_vec();
        rotated.extend_from_slice(&[0xff, 0xe1]);
        rotated.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        rotated.extend_from_slice(&exif);
        rotated.extend_from_slice(&jpeg[2..]);
        rotated
    }

    /// Preview's pictures draw an SVG apart: reads are pending (code -1)
    /// while the drawing thread runs, the finished drawing calls for a
    /// repaint, and the next read takes its picture, the one PDF and
    /// Print's pictures draw in line; then it is kept like any other.
    #[test]
    fn preview_draws_svg_pictures_apart_and_reads_them_pending_meanwhile() {
        let mut layout = Layout::default();
        layout.library_images.insert(
            77,
            LibraryImage {
                format: LibraryImageFormat::Svg,
                data: br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#.to_vec(),
            },
        );
        let object = picture(1, 77, at(0.0, 0.0, 8.0, 8.0));
        let in_line = Pictures::default().get(&layout, &object).unwrap();
        let apart = Pictures::drawn_apart();
        let (sender, repaint) = std::sync::mpsc::channel();
        apart.on_drawn(|| {
            Arc::new(move || {
                let _ = sender.send(());
            })
        });
        assert_eq!(apart.get(&layout, &object).unwrap_err().code, -1);
        repaint
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("drawn");
        let drawn = apart.get(&layout, &object).unwrap();
        assert_eq!(
            (drawn.width, drawn.height, &drawn.rgba),
            (in_line.width, in_line.height, &in_line.rgba)
        );
        assert!(Arc::ptr_eq(&apart.get(&layout, &object).unwrap(), &drawn));
        assert!(repaint.try_recv().is_err());
    }

    /// A drawing that panics is its picture's error rather than pending
    /// for ever. A drawing under way when the pictures are cleared is not
    /// kept, and the same picture asked for after the clear is drawn anew
    /// rather than waiting for it; the session is told when each ends.
    #[test]
    fn drawings_apart_end_in_an_error_or_are_drawn_anew_after_a_clear() {
        let drawing = Drawing::default();
        let (sender, repaint) = std::sync::mpsc::channel();
        drawing.on_drawn(|| {
            Arc::new(move || {
                let _ = sender.send(());
            })
        });
        let image = LibraryImage {
            format: LibraryImageFormat::Svg,
            data: b"<svg/>".to_vec(),
        };
        let wait = std::time::Duration::from_secs(30);
        let panicking = ApartKey::Picture((1, None));
        let pending = drawing.take_or_start(panicking, &image, |_| panic!("drawing"));
        assert_eq!(pending.unwrap_err().code, -1);
        repaint.recv_timeout(wait).expect("drawn");
        let failed = drawing.take_or_start(panicking, &image, |_| unreachable!());
        assert_eq!(failed.unwrap_err().code, 3);
        let cleared = ApartKey::Picture((2, None));
        let (started, under_way) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let pending = drawing.take_or_start(cleared, &image, move |_| {
            let _ = started.send(());
            let _ = gate.recv();
            Err(ReportError::unsupported("before the clear"))
        });
        assert_eq!(pending.unwrap_err().code, -1);
        under_way.recv_timeout(wait).expect("under way");
        drawing.clear();
        let anew = drawing.take_or_start(cleared, &image, |_| {
            Err(ReportError::unsupported("after the clear"))
        });
        assert_eq!(anew.unwrap_err().code, -1);
        release.send(()).unwrap();
        // The cleared drawing ends untaken, then the new one is kept.
        repaint.recv_timeout(wait).expect("cleared drawing ended");
        repaint.recv_timeout(wait).expect("new drawing ended");
        let drawn = drawing.take_or_start(cleared, &image, |_| unreachable!());
        assert_eq!(drawn.unwrap_err().message, "after the clear");
    }

    #[test]
    fn pictures_decode_like_browse_and_never_reuse_another_layouts_bytes() {
        // Browse's loader premultiplies the same oriented straight samples.
        let mut vectors = vec![
            (LibraryImageFormat::Png, png(), [40, 10]),
            (LibraryImageFormat::Jpeg, jpeg(), [64, 16]),
            (LibraryImageFormat::Jpeg, rotated_jpeg(), [16, 64]),
        ];
        vectors.extend(
            extra_pictures()
                .into_iter()
                .filter(|(_, format, _)| *format != LibraryImageFormat::Svg)
                .map(|(_, format, data)| (format, data, [28, 10])),
        );
        for (format, data, size) in vectors {
            let decoded = decode_picture(
                1,
                &LibraryImage {
                    format,
                    data: data.clone(),
                },
            )
            .unwrap();
            let browse = egui_extras::image::load_image_bytes(&data).unwrap();
            assert_eq!([decoded.width as usize, decoded.height as usize], size);
            assert_eq!(
                egui::ColorImage::from_rgba_unmultiplied(size, &decoded.rgba),
                browse
            );
            assert_eq!(decoded.opaque, format != LibraryImageFormat::Png);
        }

        // A numeric key decodes once per identical image, and a key reused
        // with other bytes (another file, layout or snapshot) is decoded anew.
        let mut first = Layout::default();
        add_pictures(&mut first);
        let mut second = first.clone();
        second.library_images.insert(
            PNG_KEY,
            LibraryImage {
                format: LibraryImageFormat::Jpeg,
                data: jpeg(),
            },
        );
        let object = first.objects.get(41).unwrap().clone();
        let pictures = Pictures::default();
        let decoded = pictures.get(&first, &object).unwrap();
        assert!(Arc::ptr_eq(
            &decoded,
            &pictures.get(&first, &object).unwrap()
        ));
        let reused = pictures.get(&second, &object).unwrap();
        assert_eq!((reused.width, reused.height), (64, 16));
        assert_eq!(pictures.get(&first, &object).unwrap().rgba, decoded.rgba);

        // SVG cache identity includes the raster size as well as source bytes.
        let small = first.objects.get(504).unwrap().clone();
        let mut large = small.clone();
        large.bounds.as_mut().unwrap().right += 28.0;
        let small_image = pictures.get(&first, &small).unwrap();
        let large_image = pictures.get(&first, &large).unwrap();
        assert_eq!((small_image.width, small_image.height), (112, 40));
        assert_eq!((large_image.width, large_image.height), (224, 40));
        assert!(
            small_image
                .rgba
                .chunks_exact(4)
                .any(|p| p[3] == 255 && p[0] > 120 && p[1] > 120 && p[2] > 120),
            "SVG includes its untinted half-white stripe"
        );
        // Text in a family that is not bundled falls back to a bundled one,
        // and ordinary layers and filters draw.
        let svg = |body: &str, size: u32| {
            let data = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{size}" height="{size}">{body}</svg>"#
            );
            decode_picture(
                9,
                &LibraryImage {
                    format: LibraryImageFormat::Svg,
                    data: data.into_bytes(),
                },
            )
        };
        for family in ["Helvetica", "serif", "monospace"] {
            let text = svg(
                &format!(r#"<rect width="40" height="40" fill="red"/><text x="1" y="30" font-family="{family}" font-size="30" fill="white">W</text>"#),
                40,
            )
            .unwrap();
            assert!(
                text.rgba
                    .chunks_exact(4)
                    .any(|p| p[0] > 200 && p[1] > 200 && p[2] > 200),
                "{family} text drawn"
            );
        }
        let layers = r#"<defs><filter id="f"><feGaussianBlur stdDeviation="2"/><feOffset dx="2"/><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs>"#.to_owned()
            + &r#"<rect width="200" height="200" fill="blue" opacity="0.5"/>"#.repeat(5)
            + r#"<g filter="url(#f)"><rect x="20" y="20" width="100" height="100"/></g>"#;
        assert!(svg(&layers, 200).is_ok());

        // What reports do not draw refuses before the tree is built: images,
        // references outside the picture and entities. What only exhausts
        // memory or time is the worker's to contain (tests/svg_worker.rs).
        for (body, reason) in [
            (
                r#"<image href="/not-a-report-resource.png" width="10" height="10"/>"#,
                "images",
            ),
            (
                r##"<use xlink:href="other.svg#a"/>"##,
                "outside the picture",
            ),
            (
                r##"<filter id="f"><feImage href="data:image/png;base64,AA"/></filter>"##,
                "outside the picture",
            ),
        ] {
            let error = svg(body, 256).unwrap_err();
            assert!(
                error.message.contains(reason),
                "{reason}: {}",
                error.message
            );
        }
        let entities = br#"<!DOCTYPE svg [<!ENTITY a "aaaaaaaa">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><desc>&a;&a;</desc></svg>"#;
        let error = decode_picture(
            9,
            &LibraryImage {
                format: LibraryImageFormat::Svg,
                data: entities.to_vec(),
            },
        )
        .unwrap_err();
        assert!(error.message.contains("entity"), "{}", error.message);
        // A tiny GIF header cannot request an enormous decoded allocation.
        assert!(decode_picture(
            9,
            &LibraryImage {
                format: LibraryImageFormat::Gif,
                data: b"GIF89a\xff\xff\xff\xff\x00\x00\x00".to_vec()
            }
        )
        .is_err());

        // Missing, damaged and unsupported images refuse with a reason.
        for (image, reason) in [
            (None, "is missing"),
            (
                Some((LibraryImageFormat::Png, png()[..40].to_vec())),
                "could not be decoded",
            ),
            (
                Some((LibraryImageFormat::Gif, b"GIF89a".to_vec())),
                "could not be decoded",
            ),
        ] {
            let mut layout = first.clone();
            layout.library_images.remove(&PNG_KEY);
            if let Some((format, data)) = image {
                layout
                    .library_images
                    .insert(PNG_KEY, LibraryImage { format, data });
            }
            let error = Pictures::default().get(&layout, &object).unwrap_err();
            assert!(error.message.contains(reason), "{}", error.message);
        }
    }
}
