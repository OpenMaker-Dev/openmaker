//! `GetAsCSS` and `GetAsSVG`: styled text as HTML spans (spec
//! `docs/re/calc-functions.md` 4, oracle answers in 9).
//!
//! Each style run (unstyled stretches included) becomes one span whose
//! `style` lists the run's attributes in a fixed order. Text is escaped:
//! CR and LF become `<br/>`, `"` `&` `<` `>` and the Latin-1, Greek and
//! symbol characters HTML 4 names become named entities (FileMaker's table,
//! [`super::tables::HTML_ENTITIES`], places `&prop;` and `&infin;` one code
//! point late), other characters outside printable ASCII decimal entities,
//! and characters outside the BMP stay as they are.

use super::tables::HTML_ENTITIES;
use super::{as_lone_surrogate, utf16_units, CharStyle, Text};
use std::fmt::Write;

/// Style flag bits (the text style keywords' values: `Bold` is 256). The
/// low half of [`CharStyle::flags`] holds the styles that are on, the high
/// half the styles explicitly set to normal (`TextStyleAdd ( t ; Plain )`
/// sets them all).
const STRIKETHROUGH: u32 = 1;
const SMALL_CAPS: u32 = 2;
const SUPERSCRIPT: u32 = 4;
const SUBSCRIPT: u32 = 8;
const UPPERCASE: u32 = 16;
const LOWERCASE: u32 = 32;
const WORD_UNDERLINE: u32 = 64;
const DOUBLE_UNDERLINE: u32 = 128;
const BOLD: u32 = 256;
const ITALIC: u32 = 512;
const UNDERLINE: u32 = 1024;
const HIGHLIGHT_YELLOW: u32 = 4096;
const CONDENSE: u32 = 8192;
const EXTEND: u32 = 16384;
const UNDERLINES: u32 = UNDERLINE | WORD_UNDERLINE | DOUBLE_UNDERLINE;

/// The size of text with no size set (and what superscript and subscript
/// halve).
const DEFAULT_SIZE: u16 = 12;

/// The CSS of one style run (spec: the formatting oracle answers in
/// calc-functions.md 9): font, size (superscript and subscript half the
/// size rounded up), color, then the styles, each `normal` or `none` when
/// set to normal.
pub fn style_css(style: &CharStyle) -> String {
    let on = |bits: u32| style.flags & 0xFFFF & bits != 0;
    let normal = |bits: u32| (style.flags >> 16) & bits != 0;
    let mut css = String::new();
    if let Some(font) = &style.font {
        let _ = write!(css, "font-family: '{font}';");
    }
    let size = style.size.unwrap_or(DEFAULT_SIZE);
    if on(SUPERSCRIPT | SUBSCRIPT) {
        let _ = write!(css, "font-size: {}px;", size.div_ceil(2));
    } else if style.size.is_some() || normal(SUPERSCRIPT | SUBSCRIPT) {
        let _ = write!(css, "font-size: {size}px;");
    }
    if let Some(color) = style.color {
        let _ = write!(css, "color: #{:06X};", color & 0xFF_FFFF);
    }
    if on(HIGHLIGHT_YELLOW) {
        css.push_str("background-color: #FFFF00;");
    } else if normal(HIGHLIGHT_YELLOW) {
        css.push_str("background-color: normal;");
    }
    if on(BOLD) {
        css.push_str("font-weight: bold;");
    } else if normal(BOLD) {
        css.push_str("font-weight: normal;");
    }
    if on(ITALIC) {
        css.push_str("font-style:italic;");
    } else if normal(ITALIC) {
        css.push_str("font-style:normal;");
    }
    match (on(UNDERLINES), on(STRIKETHROUGH)) {
        (true, true) => css.push_str("text-decoration:underline line-through ;"),
        (true, false) => css.push_str("text-decoration:underline ;"),
        (false, true) => css.push_str("text-decoration:line-through ;"),
        (false, false) if normal(UNDERLINES | STRIKETHROUGH) => {
            css.push_str("text-decoration:none;");
        }
        (false, false) => {}
    }
    if on(SMALL_CAPS) {
        css.push_str("font-variant:small-caps;");
    } else if normal(SMALL_CAPS) {
        css.push_str("font-variant:normal;");
    }
    if on(CONDENSE) {
        css.push_str("letter-spacing: -2px;");
    } else if on(EXTEND) {
        css.push_str("letter-spacing: 2px;");
    } else if normal(CONDENSE | EXTEND) {
        css.push_str("letter-spacing: normal;");
    }
    match (on(UPPERCASE), on(LOWERCASE)) {
        (true, true) => css.push_str("text-transform: capitalize;"),
        (true, false) => css.push_str("text-transform: uppercase;"),
        (false, true) => css.push_str("text-transform: lowercase;"),
        (false, false) if normal(UPPERCASE | LOWERCASE) => {
            css.push_str("text-transform: none;");
        }
        (false, false) => {}
    }
    if on(SUPERSCRIPT) {
        css.push_str("vertical-align: text-top;");
    } else if on(SUBSCRIPT) {
        css.push_str("vertical-align: text-bottom;");
    }
    css
}

/// Appends `s` escaped as span content.
fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        if let Some(unit) = as_lone_surrogate(c) {
            // As its code unit, like any other character outside ASCII.
            let _ = write!(out, "&#{unit};");
            continue;
        }
        match c {
            '\r' | '\n' => out.push_str("<br/>"),
            ' '..='~' if !matches!(c, '"' | '&' | '<' | '>') => out.push(c),
            _ if u32::from(c) > 0xFFFF => out.push(c),
            _ => {
                let unit = u32::from(c) as u16;
                match HTML_ENTITIES.binary_search_by_key(&unit, |&(code, _)| code) {
                    Ok(i) => out.push_str(HTML_ENTITIES[i].1),
                    Err(_) => {
                        let _ = write!(out, "&#{};", u32::from(c));
                    }
                }
            }
        }
    }
}

/// `GetAsCSS`: `<span style="..." >text</span>` per run; empty text gives
/// empty text.
pub fn css(text: &Text) -> String {
    css_within(text, usize::MAX).unwrap_or_default()
}

/// [`css`], or `None` once the markup passes `limit` bytes: each run's span
/// repeats its style, so the markup can be hundreds of times the text.
pub fn css_within(text: &Text, limit: usize) -> Option<String> {
    let mut out = String::new();
    for (range, style) in text.runs() {
        let _ = write!(out, "<span style=\"{}\" >", style_css(&style));
        escape(&text.as_str()[range], &mut out);
        out.push_str("</span>");
        if out.len() > limit {
            return None;
        }
    }
    Some(out)
}

/// `GetAsSVG`: a style list numbering each run with its 1-based UTF-16
/// range, then the runs as spans that name their style, lines ending in CR.
pub fn svg(text: &Text) -> String {
    svg_within(text, usize::MAX).unwrap_or_default()
}

/// [`svg`], or `None` once the markup passes `limit` bytes.
pub fn svg_within(text: &Text, limit: usize) -> Option<String> {
    let runs = text.runs();
    if runs.is_empty() {
        return Some(String::new());
    }
    let mut out = String::from("<stylelist>\r");
    let mut begin = 1;
    for (i, (range, style)) in runs.iter().enumerate() {
        let end = begin + utf16_units(&text.as_str()[range.clone()]).count() - 1;
        let _ = write!(
            out,
            "<style#{i}>\"{}\", begin: {begin}, end: {end}</style> \r",
            style_css(style)
        );
        begin = end + 1;
        if out.len() > limit {
            return None;
        }
    }
    out.push_str("</stylelist>\r<data>\r");
    for (i, (range, _)) in runs.iter().enumerate() {
        let _ = write!(out, "<span style=\"{i}\">");
        escape(&text.as_str()[range.clone()], &mut out);
        out.push_str("</span>\r");
        if out.len() > limit {
            return None;
        }
    }
    out.push_str("</data>");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(s: &str, flags: u32) -> Text {
        Text::styled(
            s,
            CharStyle {
                flags,
                ..CharStyle::default()
            },
        )
    }

    #[test]
    fn css_runs_and_escapes() {
        let mut t = styled("a", BOLD);
        t.push_str("b<é>&\"ア😀\t\r");
        assert_eq!(
            css(&t),
            "<span style=\"font-weight: bold;\" >a</span><span style=\"\" >\
             b&lt;&eacute;&gt;&amp;&quot;&#12450;😀&#9;<br/></span>"
        );
        assert_eq!(css(&Text::new()), "");
    }

    #[test]
    fn css_attribute_order() {
        let style = CharStyle {
            font: Some("Arial".into()),
            size: Some(10),
            color: Some(0x010203),
            flags: BOLD | ITALIC | UNDERLINE | STRIKETHROUGH,
        };
        assert_eq!(
            style_css(&style),
            "font-family: 'Arial';font-size: 10px;color: #010203;font-weight: bold;\
             font-style:italic;text-decoration:underline line-through ;"
        );
        assert_eq!(
            style_css(&CharStyle {
                flags: 32767,
                ..CharStyle::default()
            }),
            "font-size: 6px;background-color: #FFFF00;font-weight: bold;font-style:italic;\
             text-decoration:underline line-through ;font-variant:small-caps;\
             letter-spacing: -2px;text-transform: capitalize;vertical-align: text-top;"
        );
        let plain = CharStyle {
            flags: 0xFFFF << 16,
            ..CharStyle::default()
        };
        assert_eq!(
            style_css(&plain),
            "font-size: 12px;background-color: normal;font-weight: normal;\
             font-style:normal;text-decoration:none;font-variant:normal;\
             letter-spacing: normal;text-transform: none;"
        );
        let superscript = CharStyle {
            size: Some(13),
            flags: SUPERSCRIPT,
            ..CharStyle::default()
        };
        assert_eq!(
            style_css(&superscript),
            "font-size: 7px;vertical-align: text-top;"
        );
    }

    #[test]
    fn svg_numbers_runs_in_utf16_units() {
        let mut t = Text::plain("😀a");
        t.push(&styled("b", BOLD));
        assert_eq!(
            svg(&t),
            "<stylelist>\r<style#0>\"\", begin: 1, end: 3</style> \r\
             <style#1>\"font-weight: bold;\", begin: 4, end: 4</style> \r</stylelist>\r\
             <data>\r<span style=\"0\">😀a</span>\r<span style=\"1\">b</span>\r</data>"
        );
    }
}
