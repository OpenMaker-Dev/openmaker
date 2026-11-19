//! The display printer: a formatted stream shown as FileMaker shows it
//! (`DBCalcScan::UnTokenizeToFunctor`, calc-encoding.md 4.2). Whitespace and
//! comments are verbatim, names come from the current catalogs, literals are
//! re-quoted, numbers re-formatted from their stored value.

use crate::catalog;
use crate::formatted::Token;
use crate::lex::{is_identifier_char, is_quote, is_reserved_name, is_whitespace};
use crate::names::Names;
use crate::number::Number;
use crate::operand::Field;
use crate::parse::Context;

/// What FileMaker shows for a field, table occurrence or function the file
/// no longer has.
pub const FIELD_MISSING: &str = "<Field Missing>";
pub const TABLE_MISSING: &str = "<Table Missing>";
pub const FUNCTION_MISSING: &str = "<Function Missing>";

/// Show a formatted stream as text, with the names of `ctx.names` and the
/// file's decimal separator.
pub fn print(tokens: &[Token], ctx: &Context<'_>) -> String {
    let names = ctx.names;
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Separator => out.push(';'),
            Token::LeftParen => out.push('('),
            Token::RightParen => out.push(')'),
            Token::LeftBracket => out.push('['),
            Token::RightBracket => out.push(']'),
            Token::Whitespace(t) => out.push_str(&t.value),
            Token::BlockComment(t) => {
                out.push_str("/*");
                out.push_str(&t.value);
                out.push_str("*/");
            }
            Token::LineComment(t) => {
                out.push_str("//");
                out.push_str(&t.value);
            }
            Token::Number(n, format) => match n {
                Number::Decimal(d) if ctx.decimal_comma => {
                    out.push_str(&d.display(*format).replace('.', ","))
                }
                Number::Decimal(d) => out.push_str(&d.display(*format)),
                Number::NotANumber => out.push('?'),
                Number::Empty => {}
            },
            Token::Text(t) => literal(&t.value, &mut out),
            Token::Field(f) => field(f, names, &mut out),
            Token::Symbol(s) => name_or(Some(&s.name.value), "", &mut out),
            Token::Variable(name) => out.push_str(name),
            Token::GetSelector(sel) => {
                out.push_str(catalog::get_selector_name(*sel).unwrap_or(FUNCTION_MISSING))
            }
            Token::CustomFunction(key) => {
                let name = key
                    .id()
                    .and_then(|id| names.custom_function(id))
                    .map(|f| f.name);
                name_or(name, FUNCTION_MISSING, &mut out);
            }
            Token::PluginFunction(id) => {
                let name = catalog::builtin(*id)
                    .map(|b| b.name)
                    .or_else(|| names.plugin_function(*id).map(|f| f.name));
                name_or(name, FUNCTION_MISSING, &mut out);
            }
            Token::Op(op) => out.push_str(op.name().unwrap_or(FUNCTION_MISSING)),
        }
    }
    out
}

/// `Text::MakeLiteral`: the value in quotes, with every quote (all of
/// `"`, `“`, `”`, `„`, `‟`, `＂` close a literal) escaped by `\`, a return
/// shown as `¶` and a `¶` as `\¶`; a `\` is doubled only where it would
/// otherwise escape what follows (a `\`, quote, `¶`, return or the closing
/// quote), as FileMaker shows `\\server` as `"\\\server"`. U+E004, which a
/// return typed inside the quotes leaves in the value (5.6), shows as that
/// return.
fn literal(value: &str, out: &mut String) {
    out.push('"');
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if is_quote(c) => {
                out.push('\\');
                out.push(c);
            }
            '\\' => {
                let escapes = chars
                    .peek()
                    .map_or(true, |&n| n == '\\' || n == '¶' || n == '\r' || is_quote(n));
                out.push_str(if escapes { "\\\\" } else { "\\" });
            }
            '\r' => out.push('¶'),
            '¶' => out.push_str("\\¶"),
            '\u{E004}' => out.push('\r'),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn field(f: &Field, names: &dyn Names, out: &mut String) {
    let occurrence = f.occurrence.id();
    if f.flags & Field::TABLE_PRESENT != 0 {
        let name = occurrence.and_then(|o| names.occurrence_name(o));
        name_or(name, TABLE_MISSING, out);
        out.push_str("::");
    }
    let name = occurrence
        .zip(f.field.id())
        .and_then(|(o, id)| names.field_name(o, id));
    name_or(name, FIELD_MISSING, out);
}

fn name_or(name: Option<&str>, missing: &str, out: &mut String) {
    match name {
        Some(name) if needs_quotes(name) => {
            out.push_str("${");
            out.push_str(name);
            out.push('}');
        }
        Some(name) => out.push_str(name),
        None => out.push_str(missing),
    }
}

/// Whether a name must be written `${name}` to be read back as one name
/// (`DBCalcScan::QuoteName`): it is empty, holds a delimiter, or would
/// scan as something else (see `lex::is_reserved_name`).
pub(crate) fn needs_quotes(name: &str) -> bool {
    name.is_empty()
        || name
            .chars()
            .any(|c| !is_whitespace(c) && !is_identifier_char(c))
        || is_reserved_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{NameTable, NoNames};
    use crate::{Opcode, Text};

    #[test]
    fn literals_escape_quotes_backslashes_and_pilcrows() {
        let mut out = String::new();
        literal("a\"b\\c\rd¶e\u{E004}f„", &mut out);
        assert_eq!(out, "\"a\\\"b\\c¶d\\¶e\rf\\„\"");
        // FileMaker doubles a backslash only where it would escape.
        let shown = |v: &str| {
            let mut out = String::new();
            literal(v, &mut out);
            out
        };
        assert_eq!(shown("\\\\server\\share"), "\"\\\\\\server\\share\"");
        assert_eq!(shown("\\;"), "\"\\;\"");
        assert_eq!(shown("a\\"), "\"a\\\\\"");
        assert_eq!(shown("\\\r"), "\"\\\\¶\"");
    }

    #[test]
    fn fields_print_with_their_occurrence_when_qualified() {
        let mut names = NameTable::default();
        names.add_occurrence(0xd00002, "Contacts", 0x8002);
        names.add_field(0x8002, 2, "Last Name");
        let tokens = [
            Token::Op(Opcode(0x4e)),
            Token::Whitespace(Text::plain(" ")),
            Token::LeftParen,
            Token::Field(Field::new(0xd00002, 2, true)),
            Token::RightParen,
            Token::Op(Opcode::EQUAL),
            Token::Field(Field::new(0xd00002, 2, false)),
            Token::Op(Opcode::AND),
            Token::Field(Field::new(0xd00002, 9, false)),
        ];
        assert_eq!(
            print(&tokens, &Context::new(&names)),
            "IsEmpty (Contacts::Last Name)=Last Nameand<Field Missing>"
        );
        assert_eq!(
            print(&tokens[3..4], &Context::new(&NoNames)),
            "<Table Missing>::<Field Missing>"
        );
    }
}
