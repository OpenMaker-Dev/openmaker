//! The form fields the MBS plug-in subset's DynaPDF commands create
//! (`om_scripts::runtime::dynapdf`, docs/plugins.md), as the PDF worker
//! writes them (`pdf_assemble`). Each field is one widget annotation on its
//! page, merged with its field dictionary (PDF 1.7, 12.5.6.19 and 12.7.3.1),
//! and is listed in the document's interactive form. It has an appearance
//! stream for each state, drawn in the standard fonts Helvetica,
//! Helvetica-Bold and ZapfDingbats, which every reader has, unembedded, as
//! DynaPDF uses them for Acrobat 4 compatible fields [O, manual:
//! CreateTextField].
//!
//! [O] marks the plug-in's and DynaPDF's documentation
//! (docs/re/mbs-plugin.md 5); [I] marks OpenMaker's choice where it is
//! silent:
//! - The rectangle includes the border [O]. The border is drawn inside it
//!   at the line width, solid [I: the default style is not documented], in
//!   the border color. The background fills the rectangle, unless it has
//!   none.
//! - A text field is empty. Its font is Helvetica, or Helvetica-Bold for a
//!   bold face [I: Arial and Helvetica, which the subset draws, have the
//!   same widths], at a size or auto (0) [O].
//! - A check box's on state is named by its export value and its off state
//!   `Off` [O]. Its check is ZapfDingbats `4`, the check mark [I: the
//!   default ccCheck; the manual shows the glyph only as a picture]. The
//!   check is centered, in the text color, at the given size, or auto: 80%
//!   of the box inside the border [I]. Its default state is its state [O].
//! - A signature field is unsigned: only its frame is drawn.
//! - Each widget prints (`/F 4`) [I: the documentation says only that
//!   buttons, which this subset does not make, do not].
//! - Fields of one name and type are one field, a field group whose kids
//!   are their widgets ([`group_refusal`]).

use std::fmt::Write as _;

/// How many fields one document holds; longer names and export values
/// than these are refused.
pub const MAX_FIELDS: usize = 10_000;
pub const MAX_NAME: usize = 1024;
/// An export value is a PDF name, at most 127 bytes (PDF 1.7, C.2).
pub const MAX_EXPORT: usize = 127;
/// How long a button's caption and a JavaScript action may be, and how
/// many actions a document holds (OpenMaker's bounds).
pub const MAX_CAPTION: usize = 4096;
pub const MAX_SCRIPT: usize = 1 << 20;
pub const MAX_ACTIONS: usize = 10_000;
/// How many web links a document holds, and how long a URL may be
/// (OpenMaker's bounds).
pub const MAX_LINKS: usize = 10_000;
pub const MAX_HIGHLIGHTS: usize = 10_000;
pub const MAX_URI: usize = 64 * 1024;

/// A web link the DynaPDF commands put on a page: a link annotation whose
/// action opens `uri` (PDF 1.7, 12.5.6.5 and 12.6.4.7), its border
/// `border_width` wide in `color`.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The planned page it is on (from 0).
    pub page: u32,
    /// Lower-left x and y, upper-right x and y.
    pub rect: [f64; 4],
    pub border_width: f64,
    /// Red, green and blue from 0 to 1.
    pub color: [f64; 3],
    pub uri: String,
}

/// A highlight appearance, kept as markup or baked into page content;
/// coordinates are lower-left/upper-right.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    pub page: u32,
    pub rect: [f64; 4],
    pub color: [f64; 3],
    /// Draw the appearance into page content, without an annotation.
    pub flattened: bool,
}

/// Whether `uri` can be a link's URL: not empty, not too long, and "7 bit
/// ASCII" [O, docs: DynaPDF.WebLink], printable (a URI is an ASCII string,
/// PDF 1.7, 12.6.4.7).
pub fn uri_ok(uri: &str) -> bool {
    !uri.is_empty() && uri.len() <= MAX_URI && uri.bytes().all(|b| (0x20..0x7f).contains(&b))
}
/// The largest coordinate, border width or size a field may have, in
/// points: far past any page (PDF 1.7, C.2: 14,400 units at most), small
/// enough that no width or position computed from it overflows.
pub const MAX_COORD: f64 = 1e7;

/// Whether `name` can be a field's partial name: not empty, not too long,
/// and without a period, which separates the names of a field's ancestors
/// (PDF 1.7, 12.7.3.2) and so would make a field with a parent, which
/// OpenMaker does not make.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && !name.contains('.')
}

/// Whether `export` can name a check box's on state: not empty, not too
/// long, not `Off`, the off state's name (12.7.4.2.3), and without NUL,
/// which no PDF name holds (7.3.5).
pub fn export_ok(export: &str) -> bool {
    !export.is_empty() && export.len() <= MAX_EXPORT && export != "Off" && !export.contains('\0')
}

/// Whether a field's rectangle, lower-left then upper-right, is one: its
/// corners within [`MAX_COORD`] and in order.
pub fn rect_ok([x0, y0, x1, y1]: [f64; 4]) -> bool {
    [x0, y0, x1, y1]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= MAX_COORD)
        && x1 > x0
        && y1 > y0
}

/// A form field to write on a page of the new document.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The planned page it is on (from 0).
    pub page: u32,
    /// Its partial name, `/T`.
    pub name: String,
    pub kind: FieldKind,
    /// Lower-left x and y, upper-right x and y.
    pub rect: [f64; 4],
    pub border_width: f64,
    /// Red, green and blue from 0 to 1; `None` for none (transparent).
    pub text: [f64; 3],
    pub border: Option<[f64; 3]>,
    pub back: Option<[f64; 3]>,
    /// The font size; 0 for automatic.
    pub size: f64,
    /// The JavaScript action its widget runs when clicked (its `/A`), by
    /// its position among the bundle's actions.
    pub action: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    /// A text field, empty; `max_len` 0 for no limit.
    Text {
        bold: bool,
        multiline: bool,
        max_len: u32,
    },
    /// A check box, `checked` or not, its on state named `export`.
    CheckBox { export: String, checked: bool },
    /// A signature field, unsigned.
    Signature,
    /// A push button, its caption in Helvetica or Helvetica-Bold, drawn on
    /// page `caption_page` (a source and its page) of the PDF the desktop
    /// lays the captions out in.
    Button {
        bold: bool,
        caption: String,
        caption_page: Option<(u32, u32)>,
    },
}

/// ZapfDingbats `4` (a20), the check mark: its glyph box, per 1000 units
/// of size [Adobe's ZapfDingbats AFM: `C 52 ; WX 846 ; N a20 ; B 35 0 811
/// 705`].
const CHECK: [f64; 4] = [35.0, 0.0, 811.0, 705.0];

/// The fields after the plan in a bundle: a `u32` count, then each field:
/// - `u8` kind (0 text, 1 check box, 2 signature) and `u32` page;
/// - its name (`u32` length and UTF-8);
/// - five `f64`s: the rectangle and the border width;
/// - three `f64`s of text color;
/// - the border and background colors, each a `u8` (1 present) and three
///   `f64`s when present;
/// - the size, an `f64`;
/// - then by kind: for text, a `u8` bold, a `u8` multiline and a `u32`
///   maximum length; for a check box, the export value (`u32` length and
///   UTF-8) and a `u8` checked; for a button (kind 3), a `u8` bold, the
///   caption (`u32` length and UTF-8), and a `u8` (1 present) and the
///   caption page's source and page (`u32`s) when present;
/// - its action, a `u32` (all ones for none).
///
/// Then the actions: a `u32` count, each a JavaScript (`u32` length and
/// UTF-8). Then the links: a `u32` count, each its `u32` page, five `f64`s
/// (the rectangle and the border width), three `f64`s of color and its URL
/// (`u32` length and ASCII). Then highlights: a `u32` count; each its
/// `u32` page, four `f64` rectangle coordinates and three `f64` color
/// components, then a `u8` flag (1 for a baked appearance). Big-endian.
pub(super) fn encode(
    fields: &[Field],
    actions: &[String],
    links: &[Link],
    highlights: &[Highlight],
    out: &mut Vec<u8>,
) {
    out.extend((fields.len() as u32).to_be_bytes());
    for field in fields {
        let kind = match field.kind {
            FieldKind::Text { .. } => 0u8,
            FieldKind::CheckBox { .. } => 1,
            FieldKind::Signature => 2,
            FieldKind::Button { .. } => 3,
        };
        out.push(kind);
        out.extend(field.page.to_be_bytes());
        text(&field.name, out);
        for value in field.rect.iter().chain([&field.border_width]) {
            out.extend(value.to_be_bytes());
        }
        color(Some(field.text), false, out);
        color(field.border, true, out);
        color(field.back, true, out);
        out.extend(field.size.to_be_bytes());
        match &field.kind {
            FieldKind::Text {
                bold,
                multiline,
                max_len,
            } => {
                out.push(u8::from(*bold));
                out.push(u8::from(*multiline));
                out.extend(max_len.to_be_bytes());
            }
            FieldKind::CheckBox { export, checked } => {
                text(export, out);
                out.push(u8::from(*checked));
            }
            FieldKind::Signature => {}
            FieldKind::Button {
                bold,
                caption,
                caption_page,
            } => {
                out.push(u8::from(*bold));
                text(caption, out);
                out.push(u8::from(caption_page.is_some()));
                if let Some((source, page)) = caption_page {
                    out.extend(source.to_be_bytes());
                    out.extend(page.to_be_bytes());
                }
            }
        }
        out.extend(field.action.unwrap_or(u32::MAX).to_be_bytes());
    }
    out.extend((actions.len() as u32).to_be_bytes());
    for script in actions {
        text(script, out);
    }
    out.extend((links.len() as u32).to_be_bytes());
    for link in links {
        out.extend(link.page.to_be_bytes());
        for value in link.rect.iter().chain([&link.border_width]) {
            out.extend(value.to_be_bytes());
        }
        for value in link.color {
            out.extend(value.to_be_bytes());
        }
        text(&link.uri, out);
    }
    out.extend((highlights.len() as u32).to_be_bytes());
    for highlight in highlights {
        out.extend(highlight.page.to_be_bytes());
        for value in highlight.rect.into_iter().chain(highlight.color) {
            out.extend(value.to_be_bytes());
        }
        out.push(u8::from(highlight.flattened));
    }
}

fn text(value: &str, out: &mut Vec<u8>) {
    out.extend((value.len() as u32).to_be_bytes());
    out.extend(value.as_bytes());
}

fn color(value: Option<[f64; 3]>, optional: bool, out: &mut Vec<u8>) {
    if optional {
        out.push(u8::from(value.is_some()));
    }
    for component in value.into_iter().flatten() {
        out.extend(component.to_be_bytes());
    }
}

/// Reads a bundle's bytes in order.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn flag(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?)).filter(|v| v.is_finite())
    }
    fn text(&mut self, limit: usize) -> Option<String> {
        let len = self.u32()? as usize;
        if len > limit {
            return None;
        }
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
    fn color(&mut self) -> Option<[f64; 3]> {
        let rgb = [self.f64()?, self.f64()?, self.f64()?];
        rgb.iter().all(|c| (0.0..=1.0).contains(c)).then_some(rgb)
    }
    fn optional_color(&mut self) -> Option<Option<[f64; 3]>> {
        if self.flag()? {
            Some(Some(self.color()?))
        } else {
            Some(None)
        }
    }
}

/// [`encode`]'s bytes read back, all of `bytes`; `None` for anything
/// malformed: an unknown kind or flag, a number that is not finite, a
/// color component outside 0 to 1, a name or export value
/// [`name_ok`] or [`export_ok`] refuses, a rectangle [`rect_ok`] refuses,
/// a border or size below 0 or past [`MAX_COORD`], a check box without
/// room for its check ([`check_fits`]), or fields of one name that cannot
/// be one field ([`group_refusal`]).
type Decoded = (Vec<Field>, Vec<String>, Vec<Link>, Vec<Highlight>);

pub(super) fn decode(bytes: &[u8]) -> Option<Decoded> {
    let mut r = Reader(bytes);
    let count = r.u32()? as usize;
    if count > MAX_FIELDS {
        return None;
    }
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = r.u8()?;
        let page = r.u32()?;
        let name = r.text(MAX_NAME).filter(|n| name_ok(n))?;
        let rect = [r.f64()?, r.f64()?, r.f64()?, r.f64()?];
        let bounded = |v: &f64| (0.0..=MAX_COORD).contains(v);
        let border_width = r.f64().filter(bounded)?;
        if !rect_ok(rect) {
            return None;
        }
        let text = r.color()?;
        let border = r.optional_color()?;
        let back = r.optional_color()?;
        let size = r.f64().filter(bounded)?;
        let kind = match kind {
            0 => FieldKind::Text {
                bold: r.flag()?,
                multiline: r.flag()?,
                max_len: r.u32()?,
            },
            1 => FieldKind::CheckBox {
                export: r.text(MAX_EXPORT).filter(|e| export_ok(e))?,
                checked: r.flag()?,
            },
            2 => FieldKind::Signature,
            3 => FieldKind::Button {
                bold: r.flag()?,
                caption: r.text(MAX_CAPTION)?,
                caption_page: if r.flag()? {
                    Some((r.u32()?, r.u32()?))
                } else {
                    None
                },
            },
            _ => return None,
        };
        if matches!(kind, FieldKind::CheckBox { .. }) && !check_fits(rect, border_width, size) {
            return None;
        }
        // A caption is drawn from its page; one without is not a button
        // OpenMaker draws.
        if matches!(&kind, FieldKind::Button { caption, caption_page: None, .. } if !caption.is_empty())
        {
            return None;
        }
        let action = Some(r.u32()?).filter(|a| *a != u32::MAX);
        fields.push(Field {
            page,
            name,
            kind,
            rect,
            border_width,
            text,
            border,
            back,
            size,
            action,
        });
    }
    let count = r.u32()? as usize;
    if count > MAX_ACTIONS {
        return None;
    }
    let mut actions = Vec::with_capacity(count);
    for _ in 0..count {
        actions.push(r.text(MAX_SCRIPT)?);
    }
    let count = r.u32()? as usize;
    if count > MAX_LINKS {
        return None;
    }
    let mut links = Vec::with_capacity(count);
    for _ in 0..count {
        let page = r.u32()?;
        let rect = [r.f64()?, r.f64()?, r.f64()?, r.f64()?];
        let border_width = r.f64().filter(|w| (0.0..=MAX_COORD).contains(w))?;
        let color = r.color()?;
        let uri = r.text(MAX_URI).filter(|u| uri_ok(u))?;
        if !rect_ok(rect) {
            return None;
        }
        links.push(Link {
            page,
            rect,
            border_width,
            color,
            uri,
        });
    }
    let count = r.u32()? as usize;
    if count > MAX_HIGHLIGHTS {
        return None;
    }
    let mut highlights = Vec::with_capacity(count);
    for _ in 0..count {
        let page = r.u32()?;
        let rect = [r.f64()?, r.f64()?, r.f64()?, r.f64()?];
        let color = r.color()?;
        if !rect_ok(rect) {
            return None;
        }
        let flattened = r.flag()?;
        highlights.push(Highlight {
            page,
            rect,
            color,
            flattened,
        });
    }
    if !r.0.is_empty()
        || fields
            .iter()
            .any(|f| f.action.is_some_and(|a| a as usize >= actions.len()))
    {
        return None;
    }
    for group in groups(&fields) {
        let members: Vec<&Field> = group.iter().map(|i| &fields[*i]).collect();
        if group_refusal(&members).is_some() {
            return None;
        }
    }
    Some((fields, actions, links, highlights))
}

/// The standard fonts fields use, by the names the interactive form's
/// resources give them.
pub(super) const FONTS: [(&str, &str); 3] = [
    (
        "Helv",
        "/Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding",
    ),
    (
        "HeBo",
        "/Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding",
    ),
    (
        "ZaDb",
        "/Type /Font /Subtype /Type1 /BaseFont /ZapfDingbats",
    ),
];

/// One appearance stream of a field: the state it draws (`None` for a
/// field of one state), its content, whether it shows ZapfDingbats, and
/// the form XObject it draws as `/Cap`, if any.
pub(super) struct Appearance {
    pub state: Option<String>,
    pub content: String,
    pub dingbats: bool,
    pub caption: Option<u32>,
}

/// `value` as a PDF number.
fn num(value: f64, out: &mut String) {
    super::pdf_assemble::real(value, out);
}

fn rgb(color: [f64; 3], operator: &str, out: &mut String) {
    for component in color {
        num(component, out);
        out.push(' ');
    }
    out.push_str(operator);
    out.push('\n');
}

/// The field's width and height.
fn size(field: &Field) -> (f64, f64) {
    (field.rect[2] - field.rect[0], field.rect[3] - field.rect[1])
}

/// The background and the border, inside the field's rectangle.
fn frame(field: &Field, out: &mut String) {
    let (w, h) = size(field);
    if let Some(back) = field.back {
        rgb(back, "rg", out);
        out.push_str("0 0 ");
        num(w, out);
        out.push(' ');
        num(h, out);
        out.push_str(" re f\n");
    }
    let lw = field.border_width;
    if let Some(border) = field.border.filter(|_| lw > 0.0 && w > lw && h > lw) {
        rgb(border, "RG", out);
        num(lw, out);
        out.push_str(" w ");
        for value in [lw / 2.0, lw / 2.0, w - lw, h - lw] {
            num(value, out);
            out.push(' ');
        }
        out.push_str("re S\n");
    }
}

/// The check mark's size in a check box of `rect` and `border_width`: the
/// given `size`, or when it is 0 (automatic) 80% of the box inside the
/// border.
fn check_size(rect: [f64; 4], border_width: f64, size: f64) -> f64 {
    if size > 0.0 {
        return size;
    }
    let inner = |d: f64| (d - 2.0 * border_width).max(0.0);
    let [x0, y0, x1, y1] = CHECK;
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    0.8 * (inner(w) / ((x1 - x0) / 1000.0)).min(inner(h) / ((y1 - y0) / 1000.0))
}

/// Whether a check box of `rect` and `border_width` has room for its check
/// at `size` (0 for automatic): an automatic check needs room inside the
/// border, or it would not show.
pub fn check_fits(rect: [f64; 4], border_width: f64, size: f64) -> bool {
    check_size(rect, border_width, size) > 0.0
}

/// The field's appearance streams. A button's draws `caption`, a form
/// XObject and its box, centered in the button [O, manual: "The caption is
/// centred horizontally and vertically"].
pub(super) fn appearances(field: &Field, caption: Option<(u32, [f64; 4])>) -> Vec<Appearance> {
    let mut framed = String::new();
    frame(field, &mut framed);
    let plain = |content: String| Appearance {
        state: None,
        content,
        dingbats: false,
        caption: None,
    };
    match &field.kind {
        FieldKind::Text { .. } => vec![plain(framed + "/Tx BMC EMC\n")],
        FieldKind::Signature => vec![plain(framed)],
        FieldKind::Button { .. } => {
            let Some((form, [x0, y0, x1, y1])) = caption else {
                return vec![plain(framed)];
            };
            let (w, h) = size(field);
            let mut content = framed;
            content.push_str("q 1 0 0 1 ");
            num((w - (x1 - x0)) / 2.0 - x0, &mut content);
            content.push(' ');
            num((h - (y1 - y0)) / 2.0 - y0, &mut content);
            content.push_str(" cm /Cap Do Q\n");
            vec![Appearance {
                caption: Some(form),
                ..plain(content)
            }]
        }
        FieldKind::CheckBox { export, .. } => {
            let (w, h) = size(field);
            let s = check_size(field.rect, field.border_width, field.size);
            let [x0, y0, x1, y1] = CHECK.map(|v| v * s / 1000.0);
            let mut on = framed.clone();
            on.push_str("q BT /ZaDb ");
            num(s, &mut on);
            on.push_str(" Tf ");
            rgb(field.text, "rg", &mut on);
            num((w - (x1 - x0)) / 2.0 - x0, &mut on);
            on.push(' ');
            num((h - (y1 - y0)) / 2.0 - y0, &mut on);
            on.push_str(" Td (4) Tj ET Q\n");
            vec![
                Appearance {
                    state: Some(export.clone()),
                    content: on,
                    dingbats: true,
                    caption: None,
                },
                Appearance {
                    state: Some("Off".to_owned()),
                    ..plain(framed)
                },
            ]
        }
    }
}

/// A text string, in hex: as itself when every character is one
/// PDFDocEncoding maps to the same character (printable ASCII, tab, line
/// feed and carriage return; PDF 1.7, Annex D), else as UTF-16BE with its
/// byte order mark (7.9.2.2). An ASCII control such as U+0018 is a breve
/// in PDFDocEncoding, so it goes as UTF-16BE.
fn text_string(value: &str, out: &mut String) {
    out.push('<');
    let doc_encoded = value
        .chars()
        .all(|c| matches!(c, ' '..='~' | '\t' | '\n' | '\r'));
    if doc_encoded {
        for byte in value.bytes() {
            let _ = write!(out, "{byte:02X}");
        }
    } else {
        out.push_str("FEFF");
        for unit in value.encode_utf16() {
            let _ = write!(out, "{unit:04X}");
        }
    }
    out.push('>');
}

/// Why `members`, the fields made of one name in the order made, cannot
/// be one field, if they cannot. Fields of one name and type form a field
/// group, one field whose kids are its widgets [O, manual: "When creating
/// two fields with the same name and type then DynaPDF creates
/// automatically a field group"; "A field group cannot contain different
/// field types"]. Its widgets "should differ only in properties that
/// specify their visual appearance" (PDF 1.7, 12.7.3.2), so text fields'
/// flags must agree. Check boxes' values are their export values [O:
/// "the export value is also taken into account"]: the group's value is
/// one export value or `Off`, and each widget shows checked when its
/// export value is the group's (12.7.4.2.3); so at most one export value
/// may be made checked, and every widget of it checked [I: which value
/// DynaPDF keeps otherwise is not documented].
pub fn group_refusal(members: &[&Field]) -> Option<&'static str> {
    let first = members.first()?;
    for field in &members[1..] {
        match (&first.kind, &field.kind) {
            (
                FieldKind::Text {
                    multiline, max_len, ..
                },
                FieldKind::Text {
                    multiline: m,
                    max_len: l,
                    ..
                },
            ) => {
                if multiline != m || max_len != l {
                    return Some("The fields of a group differ only in how they look.");
                }
            }
            (FieldKind::CheckBox { .. }, FieldKind::CheckBox { .. })
            | (FieldKind::Signature, FieldKind::Signature) => {}
            // "It is not allowed to create two buttons with an identical
            // name" [O, manual].
            (FieldKind::Button { .. }, FieldKind::Button { .. }) => {
                return Some("OpenMaker creates one button of a name.");
            }
            _ => return Some("A field group holds fields of one type."),
        }
    }
    let value = group_value(members);
    let agrees = members.iter().all(|field| match &field.kind {
        FieldKind::CheckBox { export, checked } => *checked == (value == Some(export.as_str())),
        _ => true,
    });
    (!agrees).then_some("OpenMaker checks one export value of a field group, on every box of it.")
}

/// A check box group's value: the export value of its checked boxes, or
/// `None` (`Off`) when none is checked.
fn group_value<'a>(members: &[&'a Field]) -> Option<&'a str> {
    members.iter().find_map(|field| match &field.kind {
        FieldKind::CheckBox {
            export,
            checked: true,
        } => Some(export.as_str()),
        _ => None,
    })
}

/// The fields' groups: the positions of the fields of each name, in the
/// order each name was first made.
pub(super) fn groups(fields: &[Field]) -> Vec<Vec<usize>> {
    let mut order: Vec<Vec<usize>> = Vec::new();
    let mut by_name: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, field) in fields.iter().enumerate() {
        match by_name.get(field.name.as_str()) {
            Some(group) => order[*group].push(i),
            None => {
                by_name.insert(&field.name, order.len());
                order.push(vec![i]);
            }
        }
    }
    order
}

/// The entries that make a dictionary the field: its type, name, flags,
/// value and default value, and default appearance. `value` is a check box
/// group's ([`group_value`]).
fn field_entries(field: &Field, value: Option<&str>, out: &mut String) {
    match &field.kind {
        FieldKind::Text {
            multiline, max_len, ..
        } => {
            out.push_str(" /FT /Tx");
            if *multiline {
                // Multiline, bit 13 (PDF 1.7, Table 228).
                out.push_str(" /Ff 4096");
            }
            if *max_len > 0 {
                let _ = write!(out, " /MaxLen {max_len}");
            }
        }
        FieldKind::CheckBox { .. } => {
            let state = state_name(value);
            let _ = write!(out, " /FT /Btn /V {state} /DV {state}");
        }
        FieldKind::Signature => out.push_str(" /FT /Sig"),
        // Pushbutton, bit 17 (PDF 1.7, Table 226).
        FieldKind::Button { .. } => out.push_str(" /FT /Btn /Ff 65536"),
    }
    out.push_str(" /T ");
    text_string(&field.name, out);
    let _ = write!(out, " /DA ({})", default_appearance(field));
}

/// A check box state as a PDF name: an export value, or `Off`.
fn state_name(value: Option<&str>) -> String {
    let mut out = String::new();
    super::pdf_assemble::name(value.unwrap_or("Off").as_bytes(), &mut out);
    out
}

/// The field's font, size and text color, as a `/DA` string's operators.
fn default_appearance(field: &Field) -> String {
    let font = match &field.kind {
        FieldKind::Text { bold: true, .. } | FieldKind::Button { bold: true, .. } => "HeBo",
        FieldKind::Text { .. } | FieldKind::Signature | FieldKind::Button { .. } => "Helv",
        FieldKind::CheckBox { .. } => "ZaDb",
    };
    let mut da = format!("/{font} ");
    num(field.size, &mut da);
    da.push_str(" Tf ");
    da.push_str(&rgb_array(field.text));
    da.push_str("rg");
    da
}

/// The entries that make a dictionary the field's widget, on page object
/// `page`: its rectangle, border, colors, appearance streams `streams` (as
/// [`appearances`] gives them), its action object `action` and, for a
/// check box, its state, on when its export value is `value`. A button
/// does not print [O, manual: "Buttons are excluded from printing by
/// default"]; other widgets do (`/F 4`) [I].
fn widget_entries(
    field: &Field,
    page: u32,
    streams: &[(Option<String>, u32)],
    value: Option<&str>,
    action: Option<u32>,
    out: &mut String,
) {
    let flags = if matches!(field.kind, FieldKind::Button { .. }) {
        ""
    } else {
        " /F 4"
    };
    let _ = write!(
        out,
        " /Type /Annot /Subtype /Widget{flags} /P {page} 0 R /Rect ["
    );
    for (i, coordinate) in field.rect.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        num(*coordinate, out);
    }
    out.push_str("] /BS << /W ");
    num(field.border_width, out);
    out.push_str(" /S /S >> /MK <<");
    if let Some(back) = field.back {
        out.push_str(" /BG [");
        out.push_str(rgb_array(back).trim_end());
        out.push(']');
    }
    if let Some(border) = field.border {
        out.push_str(" /BC [");
        out.push_str(rgb_array(border).trim_end());
        out.push(']');
    }
    match &field.kind {
        FieldKind::CheckBox { export, .. } => {
            out.push_str(" /CA (4) >>");
            let on = value == Some(export.as_str());
            let _ = write!(out, " /AS {}", state_name(on.then_some(export.as_str())));
        }
        FieldKind::Button { caption, .. } => {
            out.push_str(" /CA ");
            text_string(caption, out);
            out.push_str(" >>");
        }
        _ => out.push_str(" >>"),
    }
    if let Some(action) = action {
        let _ = write!(out, " /A {action} 0 R");
    }
    out.push_str(" /AP << /N ");
    match streams {
        [(None, number)] => {
            let _ = write!(out, "{number} 0 R");
        }
        _ => {
            out.push_str("<<");
            for (state, number) in streams {
                out.push(' ');
                super::pdf_assemble::name(state.as_deref().unwrap_or("Off").as_bytes(), out);
                let _ = write!(out, " {number} 0 R");
            }
            out.push_str(" >>");
        }
    }
    out.push_str(" >>");
}

/// A field of one widget: its field and widget in one dictionary, on page
/// object `page`, running action object `action` when clicked.
pub(super) fn widget(
    field: &Field,
    page: u32,
    streams: &[(Option<String>, u32)],
    action: Option<u32>,
) -> String {
    let value = group_value(&[field]);
    let mut out = String::from("<<");
    field_entries(field, value, &mut out);
    widget_entries(field, page, streams, value, action, &mut out);
    out.push_str(" >>");
    out
}

/// A field group's field: the first member's type, name, flags and default
/// appearance, the group's value, and its widgets `kids` [O, manual: the
/// group field holds "the field name of the children as well as the field
/// value, default value"].
pub(super) fn group(members: &[&Field], kids: &[u32]) -> String {
    let mut out = String::from("<<");
    field_entries(members[0], group_value(members), &mut out);
    out.push_str(" /Kids [");
    for (i, kid) in kids.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{kid} 0 R");
    }
    out.push_str("] >>");
    out
}

/// A widget of field group object `parent`: no name of its own [O: the
/// "children of such a group do not contain a field name"], its own
/// rectangle, colors, border, default appearance and appearance streams,
/// and for a check box its state by the group's value.
pub(super) fn kid(
    field: &Field,
    parent: u32,
    page: u32,
    streams: &[(Option<String>, u32)],
    value: Option<&str>,
    action: Option<u32>,
) -> String {
    let mut out = format!("<< /Parent {parent} 0 R");
    widget_entries(field, page, streams, value, action, &mut out);
    let _ = write!(out, " /DA ({}) >>", default_appearance(field));
    out
}

/// A group's value, for [`kid`].
pub(super) fn value_of<'a>(members: &[&'a Field]) -> Option<&'a str> {
    group_value(members)
}

/// A link annotation's dictionary on page object `page` (PDF 1.7,
/// 12.5.6.5): its border "drawn by using the current line width, stroke
/// color" [O, manual: WebLink], solid [I: the dash pattern's default]; no
/// highlight, DynaPDF's default [O, manual: SetLinkHighlightMode,
/// "hmNone, // Default"]; a URI action (12.6.4.7), the URL an ASCII string
/// in hex. It does not print [I: no flags are documented].
pub(super) fn link(link: &Link, page: u32) -> String {
    let mut out = format!("<< /Type /Annot /Subtype /Link /P {page} 0 R /Rect [");
    for (i, value) in link.rect.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        num(*value, &mut out);
    }
    out.push_str("] /Border [0 0 ");
    num(link.border_width, &mut out);
    out.push_str("] /C [");
    out.push_str(rgb_array(link.color).trim_end());
    out.push_str("] /H /N /A << /S /URI /URI <");
    for byte in link.uri.bytes() {
        let _ = write!(out, "{byte:02X}");
    }
    out.push_str("> >> >>");
    out
}

/// A JavaScript action's dictionary (PDF 1.7, 12.6.4.16): the script as a
/// text string, written as it was given; OpenMaker never runs it.
pub(super) fn javascript(script: &str) -> String {
    let mut out = String::from("<< /S /JavaScript /JS ");
    text_string(script, &mut out);
    out.push_str(" >>");
    out
}

fn rgb_array(color: [f64; 3]) -> String {
    let mut out = String::new();
    for component in color {
        num(component, &mut out);
        out.push(' ');
    }
    out
}

/// PDF 1.7 12.5.6.10: text markup plus an explicit rectangular appearance.
/// QuadPoints use the common viewer order UL/UR/LL/LR. Exact MBS padding,
/// opacity and print behavior are unmeasured; these are OpenMaker's choices.
pub(super) fn highlight(h: &Highlight, page: u32, appearance: u32) -> (String, String, Vec<u8>) {
    let [x0, y0, x1, y1] = h.rect;
    let mut annot = format!("<< /Type /Annot /Subtype /Highlight /P {page} 0 R /Rect [");
    for value in h.rect {
        num(value, &mut annot);
        annot.push(' ');
    }
    annot.push_str("] /QuadPoints [");
    for value in [x0, y1, x1, y1, x0, y0, x1, y0] {
        num(value, &mut annot);
        annot.push(' ');
    }
    annot.push_str("] /C [");
    annot.push_str(&rgb_array(h.color));
    let _ = write!(annot, "] /F 4 /AP << /N {appearance} 0 R >> >>");
    let mut content = String::from("q /GS0 gs ");
    content.push_str(&rgb_array(h.color));
    content.push_str("rg 0 0 ");
    num(x1 - x0, &mut content);
    content.push(' ');
    num(y1 - y0, &mut content);
    content.push_str(" re f Q");
    let mut head = String::from("<< /Type /XObject /Subtype /Form /BBox [0 0 ");
    num(x1 - x0, &mut head);
    head.push(' ');
    num(y1 - y0, &mut head);
    let _ = write!(head,"] /Resources << /ExtGState << /GS0 << /Type /ExtGState /BM /Multiply /CA 1 /ca 1 >> >> >> /Length {} >>",content.len());
    (annot, head, content.into_bytes())
}
