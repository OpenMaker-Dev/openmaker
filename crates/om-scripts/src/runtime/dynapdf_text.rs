//! The text the plug-in subset's DynaPDF commands write (`super::dynapdf`,
//! docs/plugins.md): its fonts, colors and format tags, as the host lays it
//! out and draws it (`super::host::Files::render_pdf_text`).
//!
//! - **Fonts** are named as DynaPDF names them: a family (`Arial`,
//!   `Helvetica`) with a bold style, or a PostScript name in a `\FT[...]`
//!   tag (`Helvetica-Bold`, `ArialMT`). OpenMaker draws them with its own
//!   faces of the same metrics, as its reports do (Liberation Sans for
//!   Arial and Helvetica). Any other font is refused.
//! - **Format tags** ([O, DynaPDF manual, "Command tags"]):
//!   - `\FS[float]`, the font size;
//!   - `\FT[name]`, the font;
//!   - `\FC[UI32]`, the color, packed as `Blue * 65536 + Green * 256 + Red`
//!     [O, docs: DynaPDF.RGB].
//!
//!   Any other tag is refused rather than printed or ignored.
//! - A carriage return, line feed or both breaks the line.

/// The faces OpenMaker draws DynaPDF's fonts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Face {
    /// Arial or Helvetica.
    Sans,
    /// Arial or Helvetica, bold.
    SansBold,
}

impl Face {
    /// The face of a font family with a bold style.
    pub fn of_family(family: &str, bold: bool) -> Option<Face> {
        match family.trim().to_ascii_lowercase().as_str() {
            "arial" | "helvetica" => Some(if bold { Face::SansBold } else { Face::Sans }),
            _ => None,
        }
    }

    /// The face of a PostScript name, as a `\FT[...]` tag names it.
    pub fn of_postscript(name: &str) -> Option<Face> {
        match name.trim() {
            "Helvetica" | "ArialMT" | "Arial" => Some(Face::Sans),
            "Helvetica-Bold" | "Arial-BoldMT" | "Arial-Bold" => Some(Face::SansBold),
            _ => None,
        }
    }
}

/// How a text block's lines align in its rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    /// `left`, `center` or `right`, in any case. Justified text is not
    /// drawn (`None`).
    pub fn of(text: &str) -> Option<Align> {
        match text.trim().to_ascii_lowercase().as_str() {
            "left" => Some(Align::Left),
            "center" | "centre" => Some(Align::Center),
            "right" => Some(Align::Right),
            _ => None,
        }
    }
}

/// A run of text in one face, size and color; `\n` breaks its line.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub face: Face,
    pub size: f64,
    /// Red, green and blue, from 0 to 1.
    pub color: [f64; 3],
    pub text: String,
}

/// Text written into a rectangle: its left and top edge, width and height
/// (`None`: no limit) in the page's bottom-up coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: Option<f64>,
    pub align: Align,
    pub spans: Vec<Span>,
    /// Whether its glyphs' outlines, not only their advances and lines,
    /// must fit the rectangle: a button's caption, which its appearance
    /// clips.
    pub ink_inside: bool,
}

/// A packed DynaPDF color, `Blue * 65536 + Green * 256 + Red`, as red,
/// green and blue from 0 to 1.
pub fn unpack(color: u32) -> [f64; 3] {
    [
        f64::from(color & 0xff) / 255.0,
        f64::from((color >> 8) & 0xff) / 255.0,
        f64::from((color >> 16) & 0xff) / 255.0,
    ]
}

/// The style text starts in, and each tag changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub face: Face,
    pub size: f64,
    pub color: [f64; 3],
}

/// `text`'s spans, starting in `style`: its format tags read unless
/// `plain`. An error for a tag OpenMaker does not draw, or a malformed one.
pub fn spans(text: &str, mut style: Style, plain: bool) -> Result<Vec<Span>, String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut spans: Vec<Span> = Vec::new();
    let mut push = |style: Style, piece: &str| {
        if piece.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last)
                if last.face == style.face
                    && last.size == style.size
                    && last.color == style.color =>
            {
                last.text.push_str(piece);
            }
            _ => spans.push(Span {
                face: style.face,
                size: style.size,
                color: style.color,
                text: piece.to_owned(),
            }),
        }
    };
    if plain {
        push(style, &text);
        return Ok(spans);
    }
    let refused = || "OpenMaker draws only the \\FS, \\FT and \\FC format tags.".to_owned();
    let mut rest = text.as_str();
    while let Some(at) = rest.find('\\') {
        push(style, &rest[..at]);
        // A tag is two letters, `[`, its value and `]`.
        let tag = &rest[at + 1..];
        let Some(kind) = tag
            .get(..2)
            .filter(|_| tag.as_bytes().get(2) == Some(&b'['))
        else {
            return Err(refused());
        };
        let after = &tag[3..];
        let close = after.find(']').ok_or_else(refused)?;
        let value = &after[..close];
        match kind {
            "FS" => {
                style.size = value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0 && *s <= 1000.0)
                    .ok_or_else(|| format!("Invalid font size: {value}"))?;
            }
            "FT" => {
                style.face = Face::of_postscript(value)
                    .ok_or_else(|| format!("OpenMaker has no font {value}."))?;
            }
            "FC" => {
                let color = value
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid color: {value}"))?;
                style.color = unpack(color);
            }
            _ => return Err(refused()),
        }
        rest = &after[close + 1..];
    }
    push(style, rest);
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tags_change_the_font_size_and_color_of_what_follows() {
        let start = Style {
            face: Face::Sans,
            size: 12.0,
            color: [0.0; 3],
        };
        let got = spans(
            "A\\FS[9]\\FT[Helvetica-Bold]\\FC[16711680]B\rC",
            start,
            false,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].face, got[0].size, got[0].text.as_str()),
            (Face::Sans, 12.0, "A")
        );
        assert_eq!(
            (got[1].face, got[1].size, got[1].text.as_str()),
            (Face::SansBold, 9.0, "B\nC")
        );
        // Blue: 255 * 65536.
        assert_eq!(got[1].color, [0.0, 0.0, 1.0]);
        assert!(spans("\\ul#x", start, false).is_err());
        // A `]` before the `[`, or none after it, is no tag.
        assert!(spans("\\]X[", start, false).is_err());
        assert!(spans("\\FS[9", start, false).is_err());
        assert!(spans("\\FT[Times-Roman]x", start, false).is_err());
        assert_eq!(spans("\\ul#x", start, true).unwrap()[0].text, "\\ul#x");
    }
}
