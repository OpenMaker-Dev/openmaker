//! Style checks shared by the formatting and Japanese function tests.
//!
//! The harness's own style dump comes back empty, so styles are checked
//! through the engine's `GetAsSVG ( expression )`, which lists each run's
//! CSS with its first and last UTF-16 position: the inner expression is
//! evaluated through the library and every position's style is rendered
//! with the CSS rules below and compared with the engine's. Comparing
//! positions rather than runs ignores where FileMaker leaves adjacent
//! identical runs unmerged (after `TextColorRemove`, for example), which a
//! [`Text`] cannot represent.
#![allow(dead_code)]

use super::support;
use om_calc::text::style_ops::*;
use om_calc::{CharStyle, Ctx, Date, Locale, Text};

/// `GetAsCSS`'s rendering of a style, as observed: the property order is
/// fixed, superscript and subscript halve the size (12 when unset), and
/// explicitly normal styles show as `normal`/`none`.
pub fn css(s: &CharStyle) -> String {
    let (on, normal) = (s.styles_on(), s.styles_normal());
    let mut out = String::new();
    if let Some(font) = &s.font {
        out += &format!("font-family: '{font}';");
    }
    if on & POSITION != 0 {
        let size = u32::from(s.size.unwrap_or(12));
        out += &format!("font-size: {}px;", size.div_ceil(2));
    } else if let Some(size) = s.size {
        out += &format!("font-size: {size}px;");
    } else if normal & POSITION != 0 {
        out += "font-size: 12px;";
    }
    if let Some(color) = s.color {
        out += &format!("color: #{color:06X};");
    }
    let flag = |out: &mut String, bit: u16, set: &str, unset: &str| {
        if on & bit != 0 {
            *out += set;
        } else if normal & bit != 0 {
            *out += unset;
        }
    };
    flag(
        &mut out,
        HIGHLIGHT_YELLOW,
        "background-color: #FFFF00;",
        "background-color: normal;",
    );
    flag(&mut out, BOLD, "font-weight: bold;", "font-weight: normal;");
    flag(&mut out, ITALIC, "font-style:italic;", "font-style:normal;");
    let (underline, strike) = (on & UNDERLINES != 0, on & STRIKETHROUGH != 0);
    if underline || strike {
        out += "text-decoration:";
        if underline {
            out += "underline ";
        }
        if strike {
            out += "line-through ";
        }
        out += ";";
    } else if normal & (UNDERLINES | STRIKETHROUGH) != 0 {
        out += "text-decoration:none;";
    }
    flag(
        &mut out,
        SMALL_CAPS,
        "font-variant:small-caps;",
        "font-variant:normal;",
    );
    if on & CONDENSE != 0 {
        out += "letter-spacing: -2px;";
    } else if on & EXTEND != 0 {
        out += "letter-spacing: 2px;";
    } else if normal & SPACING != 0 {
        out += "letter-spacing: normal;";
    }
    match on & CASE {
        TITLECASE => out += "text-transform: capitalize;",
        UPPERCASE => out += "text-transform: uppercase;",
        LOWERCASE => out += "text-transform: lowercase;",
        _ if normal & CASE != 0 => out += "text-transform: none;",
        _ => {}
    }
    if on & SUPERSCRIPT != 0 {
        out += "vertical-align: text-top;";
    } else if on & SUBSCRIPT != 0 {
        out += "vertical-align: text-bottom;";
    }
    out
}

/// The CSS of every UTF-16 position of `text`.
pub fn positions(text: &Text) -> Vec<String> {
    let mut out = Vec::new();
    for (range, style) in text.runs() {
        let css = css(&style);
        for _ in om_calc::text::utf16_units(&text.as_str()[range]) {
            out.push(css.clone());
        }
    }
    out
}

/// The CSS of every position in the engine's `GetAsSVG` text.
pub fn svg_positions(svg: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in svg.split("<style#").skip(1) {
        let (_, rest) = entry.split_once(">\"").expect("style entry");
        let (css, rest) = rest.split_once("\", begin: ").expect("begin");
        let (begin, rest) = rest.split_once(", end: ").expect("end");
        let (end, _) = rest.split_once("</style>").expect("entry end");
        let (begin, end): (usize, usize) = (begin.parse().unwrap(), end.parse().unwrap());
        if out.len() < end {
            out.resize(end, String::new());
        }
        for slot in &mut out[begin - 1..end] {
            *slot = css.to_string();
        }
    }
    out
}

/// The `GetAsSVG` rows of a harness result file.
#[derive(Default)]
pub struct SvgTally {
    pub rows: usize,
    pub covered: usize,
    /// Rows whose expected styles are not all default (guards against a
    /// vacuous pass).
    pub styled: usize,
}

/// Checks every `GetAsSVG ( expression )` row of `path` the reader can
/// evaluate, adding failures to `failures`.
pub fn check_svg_rows(path: &str, locale: &Locale, failures: &mut Vec<String>) -> SvgTally {
    let cx = Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap());
    let mut tally = SvgTally::default();
    let tsv = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    for line in tsv.lines().filter(|l| !l.starts_with('#')) {
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        let [expr, error, _, result] = cols[..] else {
            continue;
        };
        let Some(inner) = expr
            .strip_prefix("GetAsSVG ( ")
            .and_then(|e| e.strip_suffix(" )"))
        else {
            continue;
        };
        tally.rows += 1;
        let Some((value, code)) = support::run(&support::unescape(inner), locale) else {
            continue;
        };
        tally.covered += 1;
        let got = positions(&value.to_text(&cx));
        let want = svg_positions(&support::unescape(result));
        if want.iter().any(|css| !css.is_empty()) {
            tally.styled += 1;
        }
        if got != want || code.to_string() != error {
            failures.push(format!(
                "{expr}: got {got:?} error {code}, want {want:?} error {error}"
            ));
        }
    }
    tally
}
