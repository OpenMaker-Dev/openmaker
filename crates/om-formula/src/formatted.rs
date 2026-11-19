//! The formatted stream (`KFormula`, child 5): the source as tokens in source
//! order, with whitespace and comments, names stored resolved
//! (calc-encoding.md 4.1).

use crate::catalog::Opcode;
use crate::codec::{Reader, Write};
use crate::error::{DecodeError, DecodeErrorKind};
use crate::number::Number;
use crate::operand::{Field, Key, PluginFunctionId, Symbol, Text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// `03` `;`
    Separator,
    /// `04` `(`
    LeftParen,
    /// `05` `)`
    RightParen,
    /// `06` `[`
    LeftBracket,
    /// `07` `]`
    RightBracket,
    /// `0c`: spaces, tabs, returns and no-break spaces, verbatim.
    Whitespace(Text),
    /// `0d`: `/* body */`.
    BlockComment(Text),
    /// `0e`: `// body`; the body includes its terminating return.
    LineComment(Text),
    /// `10`: a number literal and its format word ([`crate::FORMAT_FLOAT`],
    /// [`crate::FORMAT_SCIENTIFIC`]).
    Number(Number, u16),
    /// `13`: a text literal's value (escapes applied).
    Text(Text),
    /// `16`: a field, resolved to ids.
    Field(Field),
    /// `0b`: a Let/While name, parameter, or name resolved when compiled.
    Symbol(Symbol),
    /// `1a`: a `$`/`$$` variable.
    Variable(String),
    /// `9c`: the selector inside `Get ( )`.
    GetSelector(u8),
    /// `1b 14 key`: a custom function's name.
    CustomFunction(Key),
    /// `1d 0000 15 id`: a plug-in function's name.
    PluginFunction(PluginFunctionId),
    /// An operator, keyword, constant or function name by opcode; `9b` is
    /// the word `Get`.
    Op(Opcode),
}

impl Token {
    /// Whitespace and comments: tokens that only format the text.
    pub fn is_trivia(&self) -> bool {
        matches!(
            self,
            Self::Whitespace(_) | Self::BlockComment(_) | Self::LineComment(_)
        )
    }
}

/// Decode a formatted stream into its tokens.
pub fn decode(bytes: &[u8]) -> Result<Vec<Token>, DecodeError> {
    let mut r = Reader::new(bytes);
    let mut tokens = Vec::new();
    while !r.at_end() {
        tokens.push(read(&mut r)?);
    }
    Ok(tokens)
}

/// Encode tokens as a formatted stream.
pub fn encode(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for token in tokens {
        write(token, &mut out);
    }
    out
}

/// The `13` text token after `0c`, `0d` and `0e`.
fn trailing_text(r: &mut Reader<'_>) -> Result<Text, DecodeError> {
    if r.opcode()? != Opcode::TEXT {
        return Err(r.error(DecodeErrorKind::Malformed("whitespace or comment")));
    }
    r.text(true)
}

fn read(r: &mut Reader<'_>) -> Result<Token, DecodeError> {
    let start = r.pos;
    let op = r.opcode()?;
    Ok(match op {
        Opcode::SEPARATOR => Token::Separator,
        Opcode::LEFT_PAREN => Token::LeftParen,
        Opcode::RIGHT_PAREN => Token::RightParen,
        Opcode::LEFT_BRACKET => Token::LeftBracket,
        Opcode::RIGHT_BRACKET => Token::RightBracket,
        Opcode::WHITESPACE => Token::Whitespace(trailing_text(r)?),
        Opcode::BLOCK_COMMENT => Token::BlockComment(trailing_text(r)?),
        Opcode::LINE_COMMENT => Token::LineComment(trailing_text(r)?),
        Opcode::NUMBER => Token::Number(Number::read(r)?, r.u16()?),
        Opcode::TEXT => Token::Text(r.text(true)?),
        Opcode::FIELD => Token::Field(r.field()?),
        Opcode::SYMBOL => Token::Symbol(r.symbol()?),
        Opcode::VARIABLE => Token::Variable(r.text(false)?.value),
        Opcode::GET_SELECTOR => Token::GetSelector(r.u8()?),
        Opcode::CALL => {
            if r.opcode()? != Opcode::KEY {
                return Err(r.error(DecodeErrorKind::Malformed("custom function name")));
            }
            Token::CustomFunction(r.key()?)
        }
        Opcode::PLUGIN_CALL => {
            if r.u16()? != 0 || r.opcode()? != Opcode::PLUGIN_ID {
                return Err(r.error(DecodeErrorKind::Malformed("plug-in function name")));
            }
            Token::PluginFunction(r.plugin_id()?)
        }
        _ => {
            use crate::catalog::Kind;
            let valid = op.info().is_some_and(|i| {
                matches!(i.kind, Kind::Function | Kind::Operator | Kind::Constant)
            });
            if !valid {
                return Err(DecodeError {
                    offset: start,
                    kind: DecodeErrorKind::InvalidOpcode(op.0),
                });
            }
            Token::Op(op)
        }
    })
}

fn write(token: &Token, out: &mut Vec<u8>) {
    match token {
        Token::Separator => out.opcode(Opcode::SEPARATOR),
        Token::LeftParen => out.opcode(Opcode::LEFT_PAREN),
        Token::RightParen => out.opcode(Opcode::RIGHT_PAREN),
        Token::LeftBracket => out.opcode(Opcode::LEFT_BRACKET),
        Token::RightBracket => out.opcode(Opcode::RIGHT_BRACKET),
        Token::Whitespace(t) | Token::BlockComment(t) | Token::LineComment(t) => {
            out.opcode(match token {
                Token::Whitespace(_) => Opcode::WHITESPACE,
                Token::BlockComment(_) => Opcode::BLOCK_COMMENT,
                _ => Opcode::LINE_COMMENT,
            });
            out.opcode(Opcode::TEXT);
            out.text(t, true);
        }
        Token::Number(n, format) => {
            out.opcode(Opcode::NUMBER);
            n.write(out);
            out.u16(*format);
        }
        Token::Text(t) => {
            out.opcode(Opcode::TEXT);
            out.text(t, true);
        }
        Token::Field(f) => {
            out.opcode(Opcode::FIELD);
            out.field(f);
        }
        Token::Symbol(s) => {
            out.opcode(Opcode::SYMBOL);
            out.symbol(s);
        }
        Token::Variable(name) => {
            out.opcode(Opcode::VARIABLE);
            out.text(&Text::plain(name.as_str()), false);
        }
        Token::GetSelector(sel) => {
            out.opcode(Opcode::GET_SELECTOR);
            out.push(*sel);
        }
        Token::CustomFunction(key) => {
            out.opcode(Opcode::CALL);
            out.opcode(Opcode::KEY);
            out.key(key);
        }
        Token::PluginFunction(id) => {
            out.opcode(Opcode::PLUGIN_CALL);
            out.u16(0);
            out.opcode(Opcode::PLUGIN_ID);
            out.plugin_id(*id);
        }
        Token::Op(op) => out.opcode(*op),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_uuid_round_trips() {
        // calc-encoding.md 4.1: `Get ( UUID )` with single spaces.
        let bytes = [
            0x9b, 0x0c, 0x13, 0x01, 0x7a, 0x00, 0x04, 0x0c, 0x13, 0x01, 0x7a, 0x00, 0x9c, 0x65,
            0x0c, 0x13, 0x01, 0x7a, 0x00, 0x05,
        ];
        let tokens = decode(&bytes).unwrap();
        let space = Token::Whitespace(Text::plain(" "));
        assert_eq!(
            tokens,
            [
                Token::Op(Opcode::GET),
                space.clone(),
                Token::LeftParen,
                space.clone(),
                Token::GetSelector(101),
                space,
                Token::RightParen
            ]
        );
        assert_eq!(encode(&tokens), bytes);
    }
}
