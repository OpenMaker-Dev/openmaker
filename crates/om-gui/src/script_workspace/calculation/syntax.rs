//! The formula editor's text: FileMaker's line breaks, the syntax colors,
//! the cursor's line and column, error ranges, and what the operator
//! buttons insert at the cursor.
//!
//! The coloring is `om_data::formula_editor::scan`, which reads the text
//! the way FileMaker's scanner does (calc-encoding.md 6.1) but never fails:
//! text that does not compile yet is colored as far as it reads. The same
//! scan finds the name that completion completes.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use om_data::formula::Names;
use om_data::formula_editor::scan::{self, Class};
pub(super) use om_data::formula_editor::Insertion;

/// Function names and the parentheses of their calls: the teal of 42.
pub(super) const FUNCTION: Color32 = Color32::from_rgb(0x00, 0xA8, 0xA9);
/// Number literals: the magenta of 42.
pub(super) const NUMBER: Color32 = Color32::from_rgb(0xAB, 0x20, 0x79);
/// Text literals (42 shows none): a warm red that keeps apart from both.
pub(super) const TEXT_LITERAL: Color32 = Color32::from_rgb(0xC2, 0x41, 0x0C);
/// Comments, in italics (42 shows none).
pub(super) const COMMENT: Color32 = Color32::from_rgb(0x5E, 0x86, 0x4D);
/// Everything else: fields, variables, operators, separators, keywords.
pub(super) const PLAIN: Color32 = Color32::BLACK;

/// FileMaker stores a line break as CR; egui breaks lines at LF only (and
/// draws no CR). The editor shows each CR as an LF, character for
/// character, so positions are the same in both.
pub(super) fn to_editor(source: &str) -> String {
    source.replace('\r', "\n")
}

/// The editor's text as formula source: line breaks back to CR.
pub(super) fn to_source(text: &str) -> String {
    text.replace('\n', "\r")
}

/// The editor's text laid out with its colors.
pub(super) fn layout_job(
    text: &str,
    font: &FontId,
    line_height: f32,
    decimal_comma: bool,
    names: &dyn Names,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (range, class) in scan::classes(text, decimal_comma, names) {
        let color = match class {
            Class::Plain => PLAIN,
            Class::Function => FUNCTION,
            Class::Number => NUMBER,
            Class::Text => TEXT_LITERAL,
            Class::Comment => COMMENT,
        };
        job.append(
            &text[range],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                italics: class == Class::Comment,
                line_height: Some(line_height),
                ..TextFormat::default()
            },
        );
    }
    job
}

/// The line and column (from 1) of character `index`, as the dialog's
/// "L: 1 C: 1" shows the cursor.
pub(super) fn line_and_column(text: &str, index: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for c in text.chars().take(index) {
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// A UTF-16 range of `text` (how FileMaker reports an error's position,
/// calc-encoding.md 6.4) as character indices.
pub(super) fn char_range(text: &str, utf16: Range<usize>) -> Range<usize> {
    let to_char = |unit: usize| {
        let mut units = 0;
        for (n, c) in text.chars().enumerate() {
            if units >= unit {
                return n;
            }
            units += c.len_utf16();
        }
        text.chars().count()
    };
    to_char(utf16.start)..to_char(utf16.end)
}

/// The operator strip of 42, top to bottom, in its groups.
pub(super) const OPERATORS: [&[&str]; 4] = [
    &["\" \"", "( )", "&", "¶"],
    &["=", "≠", ">", "<", "≥", "≤"],
    &["+", "-", "/", "*"],
    &["not", "and", "or", "xor", "^"],
];

/// What an operator button inserts over `selection` of `text`. Quotes and
/// parentheses enclose the selection (an empty pair puts the cursor
/// inside); `not` is written before its operand; the other operators get a
/// space before them unless one is there (or a `(`) and always one after,
/// so the cursor waits where the next operand goes.
pub(super) fn operator(label: &str, text: &str, selection: Range<usize>) -> Insertion {
    let chars: Vec<char> = text.chars().collect();
    let start = selection.start.min(chars.len());
    let end = selection.end.clamp(start, chars.len());
    let selected: String = chars[start..end].iter().collect();
    let before = start.checked_sub(1).map(|i| chars[i]);
    // A space is needed after a character that would run into the operator.
    let space_before = before.is_some_and(|c| !scan::is_whitespace(c) && c != '(');
    let enclose = |open: &str, close: &str| {
        let text = format!("{open}{selected}{close}");
        let caret = if selected.is_empty() {
            open.chars().count()
        } else {
            text.chars().count()
        };
        Insertion {
            text,
            select: caret..caret,
        }
    };
    match label {
        "\" \"" => enclose("\"", "\""),
        "( )" => enclose("( ", " )"),
        "not" => Insertion::caret_after(format!("{}not ", if space_before { " " } else { "" })),
        op => Insertion::caret_after(format!("{}{op} ", if space_before { " " } else { "" })),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_convert_character_for_character() {
        let source = "Case (\r  a ;\r  b\r)";
        let text = to_editor(source);
        assert_eq!(text, "Case (\n  a ;\n  b\n)");
        assert_eq!(text.chars().count(), source.chars().count());
        assert_eq!(to_source(&text), source);
    }

    #[test]
    fn the_cursor_position_counts_lines_and_columns_from_one() {
        let text = "Case (\n  a ;\n)";
        assert_eq!(line_and_column(text, 0), (1, 1));
        assert_eq!(line_and_column(text, 6), (1, 7));
        assert_eq!(line_and_column(text, 7), (2, 1));
        assert_eq!(line_and_column(text, 10), (2, 4));
    }

    #[test]
    fn utf16_error_ranges_become_character_ranges() {
        // `😀` is two UTF-16 units and one character.
        let text = "\"😀\" + Abs ( 1 ; 2 )";
        let utf16 = 15..16;
        assert_eq!(char_range(text, utf16), 14..15);
        assert_eq!(&text.chars().skip(14).take(1).collect::<String>(), ";");
        assert_eq!(char_range("1 +", 3..3), 3..3);
    }

    #[test]
    fn operators_space_themselves_and_enclose_the_selection() {
        let insert = |text: &str, selection: Range<usize>, op: &str| {
            let mut text = text.to_owned();
            let at = operator(op, &text, selection.clone()).apply(&mut text, selection);
            (text, at)
        };
        assert_eq!(insert("a", 1..1, "&"), ("a & ".to_owned(), 4..4));
        assert_eq!(insert("a ", 2..2, "&"), ("a & ".to_owned(), 4..4));
        assert_eq!(insert("a b", 2..2, "+"), ("a + b".to_owned(), 4..4));
        assert_eq!(insert("( 1 )", 3..3, "-"), ("( 1 -  )".to_owned(), 6..6));
        assert_eq!(insert("", 0..0, "not"), ("not ".to_owned(), 4..4));
        assert_eq!(
            insert("a and", 5..5, "not"),
            ("a and not ".to_owned(), 10..10)
        );
        assert_eq!(insert("x", 0..1, "\" \""), ("\"x\"".to_owned(), 3..3));
        assert_eq!(insert("", 0..0, "\" \""), ("\"\"".to_owned(), 1..1));
        assert_eq!(insert("1 + 2", 0..5, "( )"), ("( 1 + 2 )".to_owned(), 9..9));
        assert_eq!(insert("Abs ", 4..4, "( )"), ("Abs (  )".to_owned(), 6..6));
        assert_eq!(insert("(", 1..1, "-"), ("(- ".to_owned(), 3..3));
    }
}
