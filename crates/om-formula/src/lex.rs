//! The source scanner (`DBCalcScan`, calc-encoding.md 6.1): splits formula
//! text into lexemes. Names come out unresolved; the parser resolves them
//! against its scope and the file's names, so scanning needs no context.
//!
//! Character classes, settled with FileMaker's engine (calc-encoding.md,
//! "Oracle answers (implementation)"): full-width forms count as their ASCII
//! equivalents; only the delimiters below end a name, so `#`, `@`, `:`, `|`,
//! `.` and most other punctuation are name characters; a name runs over
//! whitespace between words (`Model Year`, even `Abs x`) and stops before the
//! keywords `and`, `or`, `xor` and `not`.

use std::ops::Range;

use crate::catalog::Opcode;
use crate::error::{ErrorCode, ParseError};
use crate::formatted::Token;
use crate::number::{Decimal, FORMAT_FLOAT, FORMAT_SCIENTIFIC};
use crate::operand::Text;

/// `Unicode::ConvertToASCII` for classification: full-width forms are their
/// ASCII characters.
fn ascii(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// Whitespace: the ASCII controls tab..CR and the Unicode space separators
/// (U+00A0, U+2000..U+200A, U+3000, ...). U+2028 and U+200B are name
/// characters.
pub(crate) fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Characters that end a name: operators, punctuation, quotes, `\` and `}`.
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

/// Characters that open and close a text literal: `"`, `“`, `”`, `„`, `‟`
/// and full-width `＂` (not `″` or `〝`).
pub(crate) fn is_quote(c: char) -> bool {
    matches!(ascii(c), '"' | '\u{201C}'..='\u{201F}')
}

/// `DBCalcScan::IsIdentifierChar`: everything but whitespace and delimiters.
pub(crate) fn is_identifier_char(c: char) -> bool {
    !is_whitespace(c) && !is_delimiter(c)
}

/// Words that end a name and are operators.
fn keyword(word: &str) -> Option<Opcode> {
    Some(match word.to_ascii_lowercase().as_str() {
        "and" => Opcode::AND,
        "or" => Opcode::OR,
        "xor" => Opcode::XOR,
        "not" => Opcode::NOT,
        _ => return None,
    })
}

/// Whether a name, written plainly, would not scan back as itself. Field
/// and table occurrence names that contain a delimiter, start with a digit
/// or a period (a number), start with `$` (a variable), have whitespace at
/// either end, contain `and`, `or`, `xor` or `not` as a word, or name a
/// function that takes no arguments are written `${name}` (QuoteName).
pub(crate) fn is_reserved_name(name: &str) -> bool {
    let first = name.chars().next().map(ascii);
    matches!(first, Some('0'..='9' | '.' | '$'))
        || name.starts_with(is_whitespace)
        || name.ends_with(is_whitespace)
        || name.contains("::")
        || name.split(is_whitespace).any(|w| keyword(w).is_some())
        || crate::catalog::named(name).iter().any(|op| {
            op.info()
                .is_some_and(|i| i.nargs == crate::catalog::NArgs::NoArgs)
        })
}

/// A name as written: one part, or a table occurrence and a field
/// separated by `::`. Each part is either plain or `${...}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Name {
    pub table: Option<Part>,
    pub name: Part,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub text: String,
    pub quoted: bool,
    /// Byte range in the source, `${` and `}` included.
    pub span: Range<usize>,
}

/// A scanned unit of source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Lexeme {
    /// Punctuation, whitespace or a comment: already its formatted token.
    Token(Token),
    /// An operator (`+`, `<=`, `¶`, ...).
    Op(Opcode),
    /// `and`, `or`, `xor`, `not` as written: an operator, or a local
    /// variable of that name.
    Keyword(Opcode, String),
    Number(crate::number::Number, u16),
    Text(String),
    /// `$name` or `$$name`, dollar signs included.
    Variable(String),
    Name(Name),
    End,
}

pub(crate) struct Scanner<'a> {
    src: &'a str,
    pub pos: usize,
    /// The file's locale writes decimals with a comma: `1,5` is a number and
    /// `.` a name character (calc-encoding.md 10.1).
    decimal_comma: bool,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a str, decimal_comma: bool) -> Self {
        Self {
            src,
            pos: 0,
            decimal_comma,
        }
    }

    fn decimal_point(&self) -> char {
        if self.decimal_comma {
            ','
        } else {
            '.'
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek_char(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn error(&self, code: ErrorCode, span: Range<usize>) -> ParseError {
        ParseError { code, span }
    }

    /// The end of the text, trailing whitespace excluded.
    pub fn end(&self) -> usize {
        self.src.trim_end_matches(is_whitespace).len()
    }

    /// Whether the next significant character, after whitespace and
    /// comments, is `(`.
    pub fn followed_by_paren(&self) -> bool {
        let mut probe = Scanner { ..*self };
        loop {
            match probe.peek_char() {
                Some(c) if is_whitespace(c) => probe.pos += c.len_utf8(),
                Some('/') if probe.rest().starts_with("//") || probe.rest().starts_with("/*") => {
                    if probe.next().is_err() {
                        return false;
                    }
                }
                Some(c) => return ascii(c) == '(',
                None => return false,
            }
        }
    }

    /// Scan one lexeme and its source range.
    pub fn next(&mut self) -> Result<(Lexeme, Range<usize>), ParseError> {
        let start = self.pos;
        let Some(c) = self.peek_char() else {
            return Ok((Lexeme::End, start..start));
        };
        let lexeme = if is_whitespace(c) {
            let end = self
                .rest()
                .find(|c| !is_whitespace(c))
                .map_or(self.src.len(), |i| self.pos + i);
            let text = &self.src[self.pos..end];
            self.pos = end;
            Lexeme::Token(Token::Whitespace(Text::plain(text)))
        } else if self.rest().starts_with("//") {
            self.pos += 2;
            // To the end of the line, its return included.
            let end = self
                .rest()
                .find('\r')
                .map_or(self.src.len(), |i| self.pos + i + 1);
            let body = &self.src[self.pos..end];
            self.pos = end;
            Lexeme::Token(Token::LineComment(Text::plain(body)))
        } else if self.rest().starts_with("/*") {
            self.pos += 2;
            let body = self.block_comment(start)?;
            Lexeme::Token(Token::BlockComment(Text::plain(body)))
        } else {
            match ascii(c) {
                q if is_quote(q) => {
                    self.pos += c.len_utf8();
                    Lexeme::Text(self.literal(start)?)
                }
                a if a.is_ascii_digit() || a == self.decimal_point() => self.number(start)?,
                '$' if !self.rest().starts_with("${") => Lexeme::Variable(self.words(true)),
                _ if is_identifier_char(c) || self.rest().starts_with("${") => self.name()?,
                // No token starts with these.
                '\\' | '}' => return Err(self.error(ErrorCode::OperandExpected, start..start)),
                _ => self.punctuation(c),
            }
        };
        Ok((lexeme, start..self.pos))
    }

    /// The body of a `/* */` comment; comments nest.
    fn block_comment(&mut self, start: usize) -> Result<&'a str, ParseError> {
        let body_start = self.pos;
        let mut depth = 1;
        while depth > 0 {
            let rest = self.rest();
            if rest.is_empty() {
                return Err(self.error(ErrorCode::CommentNotTerminated, start..self.pos));
            }
            if rest.starts_with("*/") {
                depth -= 1;
                self.pos += 2;
            } else if rest.starts_with("/*") {
                depth += 1;
                self.pos += 2;
            } else {
                self.pos += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        Ok(&self.src[body_start..self.pos - 2])
    }

    /// A text literal after its opening quote (`GetLiteralString`, 6.1): any
    /// quote (`"`, `“`, `”`, full-width `＂`) ends it, and two in a row are
    /// one; `\` escapes a quote, `\` and `¶` and is kept before anything
    /// else; `¶` is a return; a return typed inside the quotes is kept as
    /// U+E004.
    fn literal(&mut self, start: usize) -> Result<String, ParseError> {
        let mut value = String::new();
        let mut chars = self.rest().char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match ascii(c) {
                q if is_quote(q) => {
                    if let Some((_, q)) = chars.next_if(|&(_, q)| is_quote(q)) {
                        value.push(q);
                        continue;
                    }
                    self.pos += i + c.len_utf8();
                    return Ok(value);
                }
                '\\' => {
                    match chars.next_if(|&(_, e)| is_quote(e) || matches!(ascii(e), '\\' | '¶')) {
                        Some((_, e)) => value.push(e),
                        None => value.push('\\'),
                    }
                }
                '¶' => value.push('\r'),
                '\r' => value.push('\u{E004}'),
                _ => value.push(c),
            }
        }
        self.pos = self.src.len();
        Err(self.error(ErrorCode::TextNotTerminated, start..self.pos))
    }

    /// A number (`GetNumber`): digits, the decimal separator and digits,
    /// `E`, a sign and digits; full-width forms count. A literal FileMaker
    /// cannot read is error 1218.
    fn number(&mut self, start: usize) -> Result<Lexeme, ParseError> {
        let point = self.decimal_point();
        let mut chars = self
            .rest()
            .char_indices()
            .map(|(i, c)| (i, ascii(c)))
            .peekable();
        let mut text = String::new();
        let mut accept = |text: &mut String, pred: &dyn Fn(char) -> bool, many: bool| {
            while let Some((_, c)) = chars.next_if(|&(_, c)| pred(c)) {
                // The literal parser reads a point whatever the locale.
                text.push(if c == point { '.' } else { c });
                if !many {
                    break;
                }
            }
        };
        let digit = |c: char| c.is_ascii_digit();
        accept(&mut text, &digit, true);
        accept(&mut text, &|c| c == point, false);
        accept(&mut text, &digit, true);
        let mut format = FORMAT_FLOAT;
        let before = text.len();
        accept(&mut text, &|c| matches!(c, 'e' | 'E'), false);
        if text.len() > before {
            format = FORMAT_SCIENTIFIC;
            accept(&mut text, &|c| matches!(c, '+' | '-'), false);
            accept(&mut text, &digit, true);
        }
        let consumed = chars.peek().map_or(self.rest().len(), |&(i, _)| i);
        self.pos += consumed;
        let value = Decimal::parse_literal(&text)
            .ok_or_else(|| self.error(ErrorCode::InvalidNumber, start..self.pos))?;
        Ok(Lexeme::Number(
            crate::number::Number::Decimal(value),
            format,
        ))
    }

    /// Name words joined by the whitespace between them, up to a delimiter,
    /// a keyword, a word starting with `::` or the end (and `${` in a name,
    /// not in a variable); trailing whitespace is left unscanned.
    fn words(&mut self, variable: bool) -> String {
        let mut end = self.pos;
        let mut i = self.pos;
        let mut first = true;
        loop {
            // A word ends at a delimiter, whitespace or a following `${`.
            let word_end = self.src[i..]
                .char_indices()
                .find(|&(n, c)| {
                    !is_identifier_char(c)
                        || !variable && n > 0 && self.src[i + n..].starts_with("${")
                })
                .map_or(self.src.len(), |(n, _)| i + n);
            let word = &self.src[i..word_end];
            if word.is_empty() || (!first && keyword(word).is_some()) {
                break;
            }
            first = false;
            end = word_end;
            let next = self.src[end..]
                .find(|c: char| !is_whitespace(c))
                .map_or(self.src.len(), |n| end + n);
            let rest = &self.src[next..];
            if next == end
                || rest.starts_with("${")
                || rest.starts_with("::")
                || keyword(word).is_some()
            {
                break;
            }
            i = next;
        }
        let text = &self.src[self.pos..end];
        self.pos = end;
        text.to_owned()
    }

    /// `${...}`: a quoted name part (`GetBrackettedName`); unterminated is
    /// 1207 at the end of the text, empty 1209 after it, a `$` name 1204.
    /// FileMaker points at the name from after `${` to after `}`.
    fn quoted(&mut self) -> Result<Part, ParseError> {
        self.pos += 2;
        let start = self.pos;
        let Some(len) = self.rest().find('}') else {
            self.pos = self.src.len();
            return Err(self.error(ErrorCode::UnbalancedParenthesis, self.pos..self.pos));
        };
        let text = self.rest()[..len].to_owned();
        self.pos += len + 1;
        if text.is_empty() {
            return Err(self.error(ErrorCode::NameMissing, self.pos..self.pos));
        }
        if text.starts_with('$') {
            return Err(self.error(ErrorCode::OperandExpected, start..self.pos));
        }
        Ok(Part {
            text,
            quoted: true,
            span: start..self.pos,
        })
    }

    fn part(&mut self) -> Result<Part, ParseError> {
        if self.rest().starts_with("${") {
            self.quoted()
        } else {
            let start = self.pos;
            Ok(Part {
                text: self.words(false),
                quoted: false,
                span: start..self.pos,
            })
        }
    }

    /// A name, possibly qualified `Table::Field`; a keyword alone is an
    /// operator.
    fn name(&mut self) -> Result<Lexeme, ParseError> {
        let first = self.part()?;
        if !first.quoted {
            if let Some(op) = keyword(&first.text) {
                return Ok(Lexeme::Keyword(op, first.text));
            }
            if let Some((table, field)) = first.text.split_once("::") {
                let split = first.span.start + table.len();
                let table = Part {
                    text: table.to_owned(),
                    quoted: false,
                    span: first.span.start..split,
                };
                let name = if field.is_empty() && self.rest().starts_with("${") {
                    self.quoted()?
                } else {
                    Part {
                        text: field.to_owned(),
                        quoted: false,
                        span: split + 2..first.span.end,
                    }
                };
                return Ok(Lexeme::Name(Name {
                    table: Some(table),
                    name,
                }));
            }
        } else if self.rest().starts_with("::") {
            self.pos += 2;
            let name = self.part()?;
            return Ok(Lexeme::Name(Name {
                table: Some(first),
                name,
            }));
        }
        Ok(Lexeme::Name(Name {
            table: None,
            name: first,
        }))
    }

    fn punctuation(&mut self, c: char) -> Lexeme {
        let rest = self.rest();
        let two = |s: &str| {
            let mut it = rest.chars().map(ascii);
            let mut want = s.chars();
            it.next() == want.next() && it.next() == want.next()
        };
        let (lexeme, len) = if two("<=") {
            (Lexeme::Op(Opcode::LESS_OR_EQUAL), 2)
        } else if two("<>") {
            (Lexeme::Op(Opcode::NOT_EQUAL), 2)
        } else if two(">=") {
            (Lexeme::Op(Opcode::GREATER_OR_EQUAL), 2)
        } else {
            let lexeme = match ascii(c) {
                '(' => Lexeme::Token(Token::LeftParen),
                ')' => Lexeme::Token(Token::RightParen),
                '[' => Lexeme::Token(Token::LeftBracket),
                ']' => Lexeme::Token(Token::RightBracket),
                ';' => Lexeme::Token(Token::Separator),
                '+' => Lexeme::Op(Opcode::ADD),
                '-' | '\u{2212}' => Lexeme::Op(Opcode::SUBTRACT),
                '*' => Lexeme::Op(Opcode::MULTIPLY),
                '/' => Lexeme::Op(Opcode::DIVIDE),
                '^' => Lexeme::Op(Opcode::POWER),
                '&' => Lexeme::Op(Opcode::CONCAT),
                '=' => Lexeme::Op(Opcode::EQUAL),
                '≠' => Lexeme::Op(Opcode::NOT_EQUAL_SIGN),
                '≤' => Lexeme::Op(Opcode::LESS_OR_EQUAL_SIGN),
                '≥' => Lexeme::Op(Opcode::GREATER_OR_EQUAL_SIGN),
                // `‹` and `›` are `<` and `>`, but never part of `<=`.
                '<' | '\u{2039}' => Lexeme::Op(Opcode::LESS),
                '>' | '\u{203A}' => Lexeme::Op(Opcode::GREATER),
                '¶' => Lexeme::Op(Opcode::PARAGRAPH),
                // Where the decimal separator is a point, `,` is the
                // argument separator and is stored as `;` (calc-encoding.md
                // 6.1; `List ( 1,5 ; 2 )` has three arguments in en-US).
                ',' if !self.decimal_comma => Lexeme::Token(Token::Separator),
                // Anything else: no operator; the parser reports it where
                // it stands.
                _ => Lexeme::Op(Opcode::EMPTY),
            };
            (lexeme, 1)
        };
        let bytes: usize = rest.chars().take(len).map(char::len_utf8).sum();
        self.pos += bytes;
        lexeme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Vec<Lexeme> {
        let mut s = Scanner::new(src, false);
        let mut out = Vec::new();
        loop {
            match s.next().unwrap().0 {
                Lexeme::End => return out,
                Lexeme::Token(Token::Whitespace(_)) => {}
                l => out.push(l),
            }
        }
    }

    /// Lexemes as text: names by their text, the rest by `Debug`.
    fn words(src: &str) -> Vec<String> {
        lex(src)
            .into_iter()
            .map(|l| match l {
                Lexeme::Name(n) => n.name.text,
                l => format!("{l:?}"),
            })
            .collect()
    }

    #[test]
    fn names_run_over_spaces_and_stop_at_keywords() {
        let and = format!("{:?}", Lexeme::Keyword(Opcode::AND, "and".into()));
        let not = format!("{:?}", Lexeme::Keyword(Opcode::NOT, "not".into()));
        assert_eq!(words("Model Year & x")[0], "Model Year");
        assert_eq!(words("x and y"), ["x", &and, "y"]);
        assert_eq!(words("Abs x"), ["Abs x"]);
        assert_eq!(words("a#b|c"), ["a#b|c"]);
        assert_eq!(words("notx"), ["notx"]);
        assert_eq!(words("not x"), [&not, "x"]);
    }

    #[test]
    fn decimal_comma_locales() {
        let mut s = Scanner::new("1,5 + a.b", true);
        let Lexeme::Number(crate::number::Number::Decimal(d), _) = s.next().unwrap().0 else {
            panic!()
        };
        assert_eq!(d, Decimal::parse_literal("1.5").unwrap());
        s.next().unwrap();
        s.next().unwrap();
        s.next().unwrap();
        assert!(matches!(s.next().unwrap().0, Lexeme::Name(n) if n.name.text == "a.b"));
    }

    #[test]
    fn qualified_and_quoted_names() {
        let Lexeme::Name(n) = &lex("Inventory Transactions::Units In | Out")[0] else {
            panic!()
        };
        assert_eq!(n.table.as_ref().unwrap().text, "Inventory Transactions");
        assert_eq!(n.name.text, "Units In | Out");
        let Lexeme::Name(n) = &lex("${Products}::${a/b}")[0] else {
            panic!()
        };
        assert!(n.table.as_ref().unwrap().quoted && n.name.quoted);
        assert_eq!(n.name.text, "a/b");
        assert_eq!(
            (n.table.as_ref().unwrap().span.clone(), n.name.span.clone()),
            (2..11, 15..19)
        );
        let mut s = Scanner::new("${x", false);
        assert_eq!(s.next().unwrap_err().code, ErrorCode::UnbalancedParenthesis);
        assert_eq!(
            Scanner::new("${}", false).next().unwrap_err().code,
            ErrorCode::NameMissing
        );
    }

    #[test]
    fn literals_apply_filemaker_escapes() {
        let text = |src: &str| match &lex(src)[0] {
            Lexeme::Text(t) => t.clone(),
            l => panic!("{l:?}"),
        };
        assert_eq!(text(r#""a\"b""#), "a\"b");
        assert_eq!(text(r#""\n""#), "\\n");
        assert_eq!(text("\"\\¶\""), "¶");
        assert_eq!(text("\"a¶b\""), "a\rb");
        assert_eq!(text("\"a\rb\nc\""), "a\u{E004}b\nc");
        assert_eq!(text(r#""a""b""#), "a\"b");
        assert_eq!(text("“ab”"), "ab");
        assert_eq!(
            Scanner::new("\"\\\"", false).next().unwrap_err().code,
            ErrorCode::TextNotTerminated
        );
    }

    #[test]
    fn numbers_comments_and_operators() {
        assert!(matches!(
            lex("1E3")[0],
            Lexeme::Number(_, FORMAT_SCIENTIFIC)
        ));
        assert_eq!(
            Scanner::new("5.", false).next().unwrap_err().code,
            ErrorCode::InvalidNumber
        );
        assert_eq!(
            lex("1 /* a /* b */ */ <> 2 // c\r"),
            [
                lex("1")[0].clone(),
                Lexeme::Token(Token::BlockComment(Text::plain(" a /* b */ "))),
                Lexeme::Op(Opcode::NOT_EQUAL),
                lex("2")[0].clone(),
                Lexeme::Token(Token::LineComment(Text::plain(" c\r"))),
            ]
        );
        assert_eq!(lex("１＋１").len(), 3);
        assert_eq!(
            Scanner::new("/* x", false).next().unwrap_err().code,
            ErrorCode::CommentNotTerminated
        );
    }
}
