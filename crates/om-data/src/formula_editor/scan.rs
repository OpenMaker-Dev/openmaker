//! Formula text read the way FileMaker's scanner reads it
//! (calc-encoding.md 6.1), without ever failing: the editor's colors and
//! the name under the cursor come from the same pieces, so text that does
//! not compile yet is read as far as it goes.

use std::ops::Range;

use crate::formula::catalog::{self, Kind, NArgs};
use crate::formula::Names;

/// What a piece of formula text is, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Plain,
    Function,
    Number,
    Text,
    Comment,
}

/// What completion needs to know of a piece besides its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    /// A name (a field, function or variable, words joined by the spaces
    /// between them); `selector` when it follows `Get (`.
    Name { selector: bool },
    /// A text literal or comment that the end of the text, or of its line
    /// for `//`, leaves open: typing at its end types into it.
    Open,
    /// Anything else: keywords, numbers, operators, whitespace, closed
    /// literals and comments, `${...}` names.
    Other,
}

/// `\t`..`\r`, spaces and the Unicode space separators (lex.rs).
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Full-width forms classify as their ASCII characters.
fn ascii(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

fn is_quote(c: char) -> bool {
    matches!(ascii(c), '"' | '\u{201C}'..='\u{201F}')
}

/// Characters that end a name (lex.rs).
fn is_delimiter(c: char) -> bool {
    matches!(
        ascii(c),
        '"' | '&'
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | '-'
            | '/'
            | ';'
            | '<'
            | '='
            | '>'
            | '['
            | '\\'
            | ']'
            | '^'
            | '}'
            | '≠'
            | '≤'
            | '≥'
            | '¶'
            | '\u{201C}'..='\u{201F}' | '\u{2039}' | '\u{203A}' | '\u{2212}'
    )
}

pub fn is_name_char(c: char) -> bool {
    !is_whitespace(c) && !is_delimiter(c)
}

fn is_keyword(word: &str) -> bool {
    ["and", "or", "xor", "not"]
        .iter()
        .any(|k| word.eq_ignore_ascii_case(k))
}

/// A function called by `name` with parentheses: built-in, design or
/// mobile, custom or plug-in.
fn is_function(name: &str, names: &dyn Names) -> bool {
    catalog::named(name)
        .iter()
        .any(|op| op.info().is_some_and(|i| i.kind == Kind::Function))
        || catalog::find_builtin(name).is_some()
        || names.find_custom_function(name).is_some()
        || names.find_plugin_function(name).is_some()
}

/// A function written without parentheses (`Pi`, `Random`, `Self`, ...).
fn is_bare_function(name: &str, names: &dyn Names) -> bool {
    catalog::named(name).iter().any(|op| {
        op.info()
            .is_some_and(|i| i.kind == Kind::Function && i.nargs == NArgs::NoArgs)
    }) || catalog::find_builtin(name).is_some_and(|b| b.max_args == Some(0))
        || names
            .find_custom_function(name)
            .and_then(|id| names.custom_function(id))
            .is_some_and(|f| f.min_args == 0)
}

/// The text split into colored pieces (byte ranges, in order, covering
/// all of it).
pub fn classes(text: &str, decimal_comma: bool, names: &dyn Names) -> Vec<(Range<usize>, Class)> {
    let mut out: Vec<(Range<usize>, Class)> = Vec::new();
    scan(text, decimal_comma, names, |range, class, _| {
        match out.last_mut() {
            Some((last, c)) if *c == class && last.end == range.start => last.end = range.end,
            _ => out.push((range, class)),
        }
    });
    out
}

/// Read `text` piece by piece: each piece's byte range (in order, covering
/// all of it), its class and what completion needs of it.
pub(super) fn scan(
    text: &str,
    decimal_comma: bool,
    names: &dyn Names,
    mut emit: impl FnMut(Range<usize>, Class, Piece),
) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let len = chars.len();
    let at = |i: usize| chars.get(i).map(|c| c.1);
    let byte = |i: usize| chars.get(i).map_or(text.len(), |c| c.0);
    let point = if decimal_comma { ',' } else { '.' };
    let digit = |c: Option<char>| c.is_some_and(|c| ascii(c).is_ascii_digit());
    // Whether each open parenthesis belongs to a function call.
    let mut parens: Vec<bool> = Vec::new();
    // The last significant piece was a function name.
    let mut after_function = false;
    // `Get (` was read: the next name is a selector.
    let mut selector_next = false;
    let mut get_called = false;
    let mut i = 0;
    while i < len {
        let start = i;
        let c = chars[i].1;
        let next = at(i + 1);
        let mut piece = Piece::Other;
        let class = if is_whitespace(c) {
            while at(i).is_some_and(is_whitespace) {
                i += 1;
            }
            emit(byte(start)..byte(i), Class::Plain, Piece::Other);
            continue;
        } else if c == '/' && next == Some('/') {
            while at(i).is_some_and(|c| c != '\n' && c != '\r') {
                i += 1;
            }
            piece = Piece::Open;
            Class::Comment
        } else if c == '/' && next == Some('*') {
            // Block comments nest.
            let mut depth = 0;
            piece = Piece::Open;
            while i < len {
                match (chars[i].1, at(i + 1)) {
                    ('/', Some('*')) => {
                        depth += 1;
                        i += 2;
                    }
                    ('*', Some('/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            piece = Piece::Other;
                            break;
                        }
                    }
                    _ => i += 1,
                }
            }
            Class::Comment
        } else if is_quote(c) {
            // `\` escapes the next character; `""` is a quote inside.
            i += 1;
            piece = Piece::Open;
            while i < len {
                let ch = chars[i].1;
                if ch == '\\' {
                    i += 2;
                } else if is_quote(ch) {
                    if at(i + 1).is_some_and(is_quote) {
                        i += 2;
                    } else {
                        i += 1;
                        piece = Piece::Other;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            i = i.min(len);
            Class::Text
        } else if digit(Some(c)) || (c == point && digit(next)) {
            while digit(at(i)) {
                i += 1;
            }
            if at(i) == Some(point) {
                i += 1;
                while digit(at(i)) {
                    i += 1;
                }
            }
            if matches!(at(i), Some('e' | 'E')) {
                let sign = usize::from(matches!(at(i + 1), Some('+' | '-')));
                if digit(at(i + 1 + sign)) {
                    i += 1 + sign;
                    while digit(at(i)) {
                        i += 1;
                    }
                }
            }
            Class::Number
        } else if c == '$' && next == Some('{') {
            // `${name}`: a quoted name.
            while at(i).is_some_and(|c| c != '}') {
                i += 1;
            }
            i = (i + 1).min(len);
            Class::Plain
        } else if is_name_char(c) {
            // A name runs over the whitespace between its words and stops
            // before a keyword (calc-encoding.md 6.1).
            let word = |mut j: usize| {
                while at(j).is_some_and(is_name_char) {
                    j += 1;
                }
                j
            };
            let mut end = word(i);
            if is_keyword(&text[byte(i)..byte(end)]) {
                i = end;
                after_function = false;
                emit(byte(start)..byte(i), Class::Plain, Piece::Other);
                continue;
            }
            loop {
                let mut k = end;
                while at(k).is_some_and(is_whitespace) {
                    k += 1;
                }
                if !at(k).is_some_and(is_name_char) {
                    break;
                }
                let w = word(k);
                if is_keyword(&text[byte(k)..byte(w)]) {
                    break;
                }
                end = w;
            }
            i = end;
            let name = &text[byte(start)..byte(end)];
            let mut k = end;
            while at(k).is_some_and(is_whitespace) {
                k += 1;
            }
            let call = at(k) == Some('(');
            let selector = selector_next;
            let function = if selector_next {
                selector_next = false;
                catalog::find_get_selector(name).is_some()
            } else if call {
                is_function(name, names)
            } else {
                is_bare_function(name, names)
            };
            get_called = function && call && name.eq_ignore_ascii_case("get");
            after_function = function && call;
            emit(
                byte(start)..byte(i),
                if function {
                    Class::Function
                } else {
                    Class::Plain
                },
                Piece::Name { selector },
            );
            continue;
        } else if c == '(' {
            i += 1;
            parens.push(after_function);
            selector_next = get_called;
            if after_function {
                Class::Function
            } else {
                Class::Plain
            }
        } else if c == ')' {
            i += 1;
            if parens.pop() == Some(true) {
                Class::Function
            } else {
                Class::Plain
            }
        } else {
            i += 1;
            Class::Plain
        };
        after_function = false;
        get_called = false;
        emit(byte(start)..byte(i), class, piece);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::{NameTable, NoNames};

    fn colored(text: &str) -> Vec<(&str, Class)> {
        classes(text, false, &NoNames)
            .into_iter()
            .map(|(r, c)| (text[r].trim(), c))
            .filter(|(t, _)| !t.is_empty())
            .collect()
    }

    #[test]
    fn functions_and_their_parentheses_numbers_text_and_comments_are_colored() {
        use Class::*;
        assert_eq!(
            colored("Left ( Last Name ; 1 ) & \"a\"\"b\" // initial"),
            [
                ("Left", Function),
                ("(", Function),
                ("Last Name ;", Plain),
                ("1", Number),
                (")", Function),
                ("&", Plain),
                ("\"a\"\"b\"", Text),
                ("// initial", Comment),
            ]
        );
        // A grouping parenthesis is not a call's; `not` is a keyword, and a
        // name that only starts like a function (`Last Name`) is a field.
        assert_eq!(
            colored("not ( IsEmpty ( Last Name ) ) /* a /* nested */ b */ 2.5E-3"),
            [
                ("not (", Plain),
                ("IsEmpty", Function),
                ("(", Function),
                ("Last Name", Plain),
                (")", Function),
                (")", Plain),
                ("/* a /* nested */ b */", Comment),
                ("2.5E-3", Number),
            ]
        );
        // Get selectors and functions without parentheses.
        assert_eq!(
            colored("Get ( ScriptParameter ) + Pi + Pi Value"),
            [
                ("Get", Function),
                ("(", Function),
                ("ScriptParameter", Function),
                (")", Function),
                ("+", Plain),
                ("Pi", Function),
                ("+ Pi Value", Plain),
            ]
        );
    }

    #[test]
    fn unfinished_text_is_colored_as_far_as_it_reads() {
        use Class::*;
        assert_eq!(colored("\"abc"), [("\"abc", Text)]);
        assert_eq!(colored("/* open"), [("/* open", Comment)]);
        assert_eq!(
            colored("Abs ( 1"),
            [("Abs", Function), ("(", Function), ("1", Number)]
        );
        assert_eq!(colored("${abc"), [("${abc", Plain)]);
        // With a decimal comma, `1,5` is one number.
        let text = "1,5";
        assert_eq!(classes(text, true, &NoNames), [(0..3, Number)]);
    }

    #[test]
    fn custom_functions_are_functions() {
        let mut names = NameTable::default();
        names.add_custom_function(1, "Initials", 1);
        let text = "Initials ( x )";
        assert_eq!(classes(text, false, &names)[0], (0..8, Class::Function));
    }
}
