//! The Polish stream (`KPolish`, child 6): compiled prefix code, the form
//! FileMaker evaluates (calc-encoding.md 3).

use crate::catalog::Opcode;
use crate::codec::{Reader, Result, Write};
use crate::error::{DecodeError, DecodeErrorKind};
use crate::expr::{call_form, CallForm, Expr};
use crate::number::Number;
use crate::operand::{Key, PluginFunctionId};

/// `GetNextOp` rejects counts above 1000 (3.1).
const MAX_COUNT: u16 = 1000;
/// Nesting limit of the decoder. The deepest of the 60,573 stored formulas
/// in the templates, test project and MBS examples is 54.
pub const MAX_DEPTH: usize = 150;

/// Bounds a caller puts on a decode. They are checked while reading, so a
/// hostile stream is refused before its tree is built or the stack is
/// exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Deepest nesting accepted (the outermost node is depth 0).
    pub depth: usize,
    /// Most nodes accepted.
    pub nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            depth: MAX_DEPTH,
            nodes: usize::MAX,
        }
    }
}

/// Decode a Polish stream into its tree. Every byte must belong to the one
/// expression.
pub fn decode(bytes: &[u8]) -> std::result::Result<Expr, DecodeError> {
    decode_with(bytes, Limits::default())
}

/// [`decode`] within `limits`; a stream past them is
/// `Malformed("nesting depth")` or `Malformed("too many nodes")`.
pub fn decode_with(bytes: &[u8], limits: Limits) -> std::result::Result<Expr, DecodeError> {
    let mut r = Reader::new(bytes);
    r.max_depth = limits.depth.min(MAX_DEPTH);
    r.nodes_left = limits.nodes;
    let expr = read(&mut r, 0)?;
    if !r.at_end() {
        return Err(r.error(DecodeErrorKind::TrailingBytes));
    }
    Ok(expr)
}

/// Encode a tree as a Polish stream.
pub fn encode(expr: &Expr) -> Vec<u8> {
    let mut out = Vec::new();
    write(expr, &mut out);
    out
}

fn count(r: &mut Reader<'_>) -> Result<u16> {
    let n = r.u16()?;
    if n > MAX_COUNT {
        return Err(DecodeError {
            offset: r.pos - 2,
            kind: DecodeErrorKind::CountTooLarge(n),
        });
    }
    Ok(n)
}

/// A node's metadata, read before its operands: what it is and how many
/// operands its header declares.
enum Head {
    /// A node without operands, complete.
    Leaf(Expr),
    List(u16),
    Call(Opcode, u16),
    CustomCall {
        tail: bool,
        function: Key,
        args: u16,
    },
    PluginCall {
        function: PluginFunctionId,
        args: u16,
    },
    Case(u16),
    Choose(u16),
    Let(u16),
    /// The logic count follows the condition.
    While {
        initial: u16,
        logic: Option<u16>,
    },
    Evaluate {
        args: u16,
        need_field_ref: u16,
    },
    GetField {
        need_field_ref: u16,
    },
}

impl Head {
    /// The operands the stream holds for the node, as far as its counts
    /// have been read, in stream order.
    fn operands(&self) -> usize {
        let n = |n: &u16| usize::from(*n);
        match self {
            Head::Leaf(_) => 0,
            Head::List(args)
            | Head::Call(_, args)
            | Head::CustomCall { args, .. }
            | Head::PluginCall { args, .. }
            | Head::Evaluate { args, .. } => n(args),
            Head::Case(pairs) => 2 * n(pairs),
            Head::Choose(results) => 1 + n(results),
            Head::Let(bindings) => n(bindings) + 1,
            Head::While { initial, logic } => {
                n(initial) + 1 + logic.as_ref().map_or(0, |l| n(l) + 1)
            }
            Head::GetField { .. } => 1,
        }
    }
}

fn read(r: &mut Reader<'_>, depth: usize) -> Result<Expr> {
    // Operands are read here only, one at a time, into storage that grows
    // as they are: a count in a header reserves nothing, and decoding
    // temporaries stay out of this recursive frame.
    let mut head = read_head(r, depth)?;
    let mut operands = Vec::new();
    while operands.len() < head.operands() {
        operands.push(read(r, depth + 1)?);
        read_logic_count(r, &mut head, operands.len())?;
    }
    Ok(assemble(head, operands))
}

/// Count a node at `depth` against the limits.
fn enter(r: &mut Reader<'_>, depth: usize) -> Result<()> {
    if depth > r.max_depth {
        return Err(r.error(DecodeErrorKind::Malformed("nesting depth")));
    }
    r.nodes_left = r
        .nodes_left
        .checked_sub(1)
        .ok_or_else(|| r.error(DecodeErrorKind::Malformed("too many nodes")))?;
    Ok(())
}

/// Read While's logic count once its initial bindings and condition, the
/// first `read` operands, are.
fn read_logic_count(r: &mut Reader<'_>, head: &mut Head, read: usize) -> Result<()> {
    if let Head::While {
        initial,
        logic: logic @ None,
    } = head
    {
        if read == usize::from(*initial) + 1 {
            *logic = Some(count(r)?);
        }
    }
    Ok(())
}

/// Decode the metadata of a node at `depth`, up to its first operand.
fn read_head(r: &mut Reader<'_>, depth: usize) -> Result<Head> {
    enter(r, depth)?;
    let start = r.pos;
    let op = r.opcode()?;
    let invalid = || DecodeError {
        offset: start,
        kind: DecodeErrorKind::InvalidOpcode(op.0),
    };
    let leaf = match op {
        Opcode::EMPTY => Expr::Empty,
        Opcode::SYMBOL => Expr::Symbol(r.symbol()?),
        Opcode::NUMBER => Expr::Number(Number::read(r)?),
        Opcode::BOOLEAN => Expr::Boolean(Number::read(r)?),
        Opcode::TEXT => Expr::Text(r.text(true)?),
        Opcode::FIELD => Expr::Field(r.field()?),
        Opcode::FIELD_REF => Expr::FieldRef(r.field()?),
        Opcode::LOCAL => Expr::Local(r.u16()?),
        Opcode::VARIABLE => Expr::Variable(r.text(false)?.value),
        Opcode::GET => Expr::Get(r.u8()?),
        Opcode::LIST => return Ok(Head::List(count(r)?)),
        Opcode::CALL | Opcode::TAIL_CALL => {
            let args = count(r)?;
            if r.opcode()? != Opcode::KEY {
                return Err(r.error(DecodeErrorKind::Malformed("custom function call")));
            }
            return Ok(Head::CustomCall {
                tail: op == Opcode::TAIL_CALL,
                function: r.key()?,
                args,
            });
        }
        Opcode::PLUGIN_CALL => {
            let args = count(r)?;
            if r.opcode()? != Opcode::PLUGIN_ID {
                return Err(r.error(DecodeErrorKind::Malformed("plug-in call")));
            }
            return Ok(Head::PluginCall {
                function: r.plugin_id()?,
                args,
            });
        }
        Opcode::CASE => return Ok(Head::Case(count(r)?)),
        Opcode::CHOOSE => return Ok(Head::Choose(count(r)?)),
        Opcode::LET => return Ok(Head::Let(count(r)?)),
        Opcode::WHILE => {
            return Ok(Head::While {
                initial: count(r)?,
                logic: None,
            })
        }
        Opcode::EVALUATE => {
            let args = count(r)?;
            return Ok(Head::Evaluate {
                args,
                need_field_ref: r.u16()?,
            });
        }
        Opcode::GET_FIELD => {
            return Ok(Head::GetField {
                need_field_ref: r.u16()?,
            })
        }
        _ => {
            let args = match call_form(op).ok_or_else(invalid)? {
                CallForm::Fixed(n) => u16::from(n),
                CallForm::Swapped => 2,
                CallForm::Counted => count(r)?,
            };
            return Ok(Head::Call(op, args));
        }
    };
    Ok(Head::Leaf(leaf))
}

/// The node from its metadata and its operands in stream order, of the
/// number [`Head::operands`] gives.
fn assemble(head: Head, mut operands: Vec<Expr>) -> Expr {
    match head {
        Head::Leaf(expr) => expr,
        Head::List(_) => {
            operands.reverse();
            Expr::List(operands)
        }
        Head::Call(op, _) => {
            if matches!(call_form(op), Some(CallForm::Swapped | CallForm::Counted)) {
                operands.reverse();
            }
            Expr::Call { op, args: operands }
        }
        Head::CustomCall { tail, function, .. } => {
            operands.reverse();
            Expr::CustomCall {
                tail,
                function,
                args: operands,
            }
        }
        Head::PluginCall { function, .. } => {
            operands.reverse();
            Expr::PluginCall {
                function,
                args: operands,
            }
        }
        Head::Case(_) => {
            let mut pairs = Vec::with_capacity(operands.len() / 2);
            let mut it = operands.into_iter();
            while let (Some(test), Some(result)) = (it.next(), it.next()) {
                pairs.push((test, result));
            }
            Expr::Case(pairs)
        }
        Head::Choose(_) => {
            let results = operands.split_off(1);
            Expr::Choose {
                test: last(&mut operands),
                results,
            }
        }
        Head::Let(_) => {
            let body = last(&mut operands);
            Expr::Let {
                bindings: operands,
                body,
            }
        }
        Head::While { initial, .. } => {
            let result = last(&mut operands);
            let logic = operands.split_off(usize::from(initial) + 1);
            let condition = last(&mut operands);
            Expr::While {
                initial: operands,
                condition,
                logic,
                result,
            }
        }
        Head::Evaluate { need_field_ref, .. } => Expr::Evaluate {
            args: operands,
            need_field_ref,
        },
        Head::GetField { need_field_ref } => Expr::GetField {
            arg: last(&mut operands),
            need_field_ref,
        },
    }
}

/// The last operand, which the header declared.
fn last(operands: &mut Vec<Expr>) -> Box<Expr> {
    Box::new(operands.pop().expect("declared operand"))
}

fn write_reversed(args: &[Expr], out: &mut Vec<u8>) {
    for arg in args.iter().rev() {
        write(arg, out);
    }
}

fn write(expr: &Expr, out: &mut Vec<u8>) {
    match expr {
        Expr::Empty => out.opcode(Opcode::EMPTY),
        Expr::Number(n) => {
            out.opcode(Opcode::NUMBER);
            n.write(out);
        }
        Expr::Boolean(n) => {
            out.opcode(Opcode::BOOLEAN);
            n.write(out);
        }
        Expr::Text(t) => {
            out.opcode(Opcode::TEXT);
            out.text(t, true);
        }
        Expr::Field(f) => {
            out.opcode(Opcode::FIELD);
            out.field(f);
        }
        Expr::FieldRef(f) => {
            out.opcode(Opcode::FIELD_REF);
            out.field(f);
        }
        Expr::Symbol(s) => {
            out.opcode(Opcode::SYMBOL);
            out.symbol(s);
        }
        Expr::Local(slot) => {
            out.opcode(Opcode::LOCAL);
            out.u16(*slot);
        }
        Expr::List(items) => {
            out.opcode(Opcode::LIST);
            out.u16(items.len() as u16);
            write_reversed(items, out);
        }
        Expr::Variable(name) => {
            out.opcode(Opcode::VARIABLE);
            out.text(&crate::operand::Text::plain(name.as_str()), false);
        }
        Expr::Get(selector) => {
            out.opcode(Opcode::GET);
            out.push(*selector);
        }
        Expr::Call { op, args } => {
            out.opcode(*op);
            match call_form(*op) {
                Some(CallForm::Swapped) => write_reversed(args, out),
                Some(CallForm::Counted) => {
                    out.u16(args.len() as u16);
                    write_reversed(args, out);
                }
                _ => args.iter().for_each(|a| write(a, out)),
            }
        }
        Expr::Case(pairs) => {
            out.opcode(Opcode::CASE);
            out.u16(pairs.len() as u16);
            for (test, result) in pairs {
                write(test, out);
                write(result, out);
            }
        }
        Expr::Choose { test, results } => {
            out.opcode(Opcode::CHOOSE);
            out.u16(results.len() as u16);
            write(test, out);
            results.iter().for_each(|a| write(a, out));
        }
        Expr::Let { bindings, body } => {
            out.opcode(Opcode::LET);
            out.u16(bindings.len() as u16);
            bindings.iter().for_each(|a| write(a, out));
            write(body, out);
        }
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => {
            out.opcode(Opcode::WHILE);
            out.u16(initial.len() as u16);
            initial.iter().for_each(|a| write(a, out));
            write(condition, out);
            out.u16(logic.len() as u16);
            logic.iter().for_each(|a| write(a, out));
            write(result, out);
        }
        Expr::Evaluate {
            args,
            need_field_ref,
        } => {
            out.opcode(Opcode::EVALUATE);
            out.u16(args.len() as u16);
            out.u16(*need_field_ref);
            args.iter().for_each(|a| write(a, out));
        }
        Expr::GetField {
            arg,
            need_field_ref,
        } => {
            out.opcode(Opcode::GET_FIELD);
            out.u16(*need_field_ref);
            write(arg, out);
        }
        Expr::CustomCall {
            tail,
            function,
            args,
        } => {
            out.opcode(if *tail {
                Opcode::TAIL_CALL
            } else {
                Opcode::CALL
            });
            out.u16(args.len() as u16);
            out.opcode(Opcode::KEY);
            out.key(function);
            write_reversed(args, out);
        }
        Expr::PluginCall { function, args } => {
            out.opcode(Opcode::PLUGIN_CALL);
            out.u16(args.len() as u16);
            out.opcode(Opcode::PLUGIN_ID);
            out.plugin_id(*function);
            write_reversed(args, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operand::{Field, Key};

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn limits_refuse_deep_and_large_streams_while_reading() {
        // Not ( Not ( ... Get ( CurrentDate ) ) ): 40 levels, 41 nodes.
        let mut deep = vec![0x1e; 40];
        deep.extend([0x9b, 2]);
        assert!(decode(&deep).is_ok());
        let too_deep = Limits {
            depth: 32,
            nodes: usize::MAX,
        };
        assert_eq!(
            decode_with(&deep, too_deep).unwrap_err().kind,
            DecodeErrorKind::Malformed("nesting depth")
        );
        let too_many = Limits {
            depth: 100,
            nodes: 40,
        };
        assert_eq!(
            decode_with(&deep, too_many).unwrap_err().kind,
            DecodeErrorKind::Malformed("too many nodes")
        );
        let exact = Limits {
            depth: 40,
            nodes: 41,
        };
        assert!(decode_with(&deep, exact).is_ok());
    }

    #[test]
    fn contacts_initial_round_trips() {
        // calc-encoding.md 3.1: Case ( not IsEmpty ( Last Name ) ; Left (
        // Last Name ; 1 ) ; not IsEmpty ( First Name ) ; Left ( First Name ;
        // 1 ) ), fields 2 and 1 of occurrence d0 00 02.
        let one = "10 02 0001 0010 00000001 00000000 00000000";
        let bytes = hex(&format!(
            "4c 0002 1e 4e 16 00 04 03d00002 02 0102 0001 57 16 00 04 03d00002 02 0102 0001 {one}
                     1e 4e 16 00 04 03d00002 02 0101 0001 57 16 00 04 03d00002 02 0101 0001 {one}"
        ));
        let expr = decode(&bytes).unwrap();
        let Expr::Case(pairs) = &expr else {
            panic!("{expr:?}")
        };
        assert_eq!(pairs.len(), 2);
        let last_name = Expr::Field(Field::new(0xd00002, 2, false));
        assert_eq!(
            pairs[0].0,
            Expr::call(Opcode::NOT, vec![Expr::call(Opcode(0x4e), vec![last_name])])
        );
        assert_eq!(encode(&expr), bytes);
    }

    #[test]
    fn varargs_store_their_arguments_last_first() {
        // List ( "a" ; "b" ) is `87 0002 "b" "a"`.
        let bytes = hex("87 0002 13 01 38 00 13 01 3b 00");
        let expr = decode(&bytes).unwrap();
        let Expr::Call { args, .. } = &expr else {
            panic!()
        };
        assert_eq!(args[0], Expr::Text(crate::operand::Text::plain("a")));
        assert_eq!(encode(&expr), bytes);
    }

    #[test]
    fn get_and_design_functions() {
        // `1d 0001 15 464d4452 000a 9b 05` = LayoutNames ( Get ( FileName ) ).
        let bytes = hex("1d 0001 15 464d4452 000a 9b 05");
        assert_eq!(
            decode(&bytes).unwrap(),
            Expr::PluginCall {
                function: crate::PluginFunctionId {
                    plugin: *b"FMDR",
                    function: 10
                },
                args: vec![Expr::Get(5)],
            }
        );
        assert_eq!(decode(&hex("9b 65")).unwrap(), Expr::Get(101));
    }

    #[test]
    fn custom_calls_and_locals() {
        let bytes = hex("1c 0002 14 02 01 04 18 0001 18 0000");
        let expr = decode(&bytes).unwrap();
        assert_eq!(
            expr,
            Expr::CustomCall {
                tail: true,
                function: Key(vec![1, 4]),
                args: vec![Expr::Local(0), Expr::Local(1)],
            }
        );
        assert_eq!(encode(&expr), bytes);
    }

    #[test]
    fn malformed_streams_are_rejected() {
        let bytes = hex("57 13 01 38 00");
        assert_eq!(decode(&bytes).unwrap_err().kind, DecodeErrorKind::Truncated);
        assert!(decode(&hex("ff 00")).is_err());
        assert!(decode(&hex("03")).is_err());
        assert!(decode(&hex("87 03e9")).is_err());
        assert_eq!(
            decode(&hex("9b 65 00")).unwrap_err().kind,
            DecodeErrorKind::TrailingBytes
        );
    }
}
