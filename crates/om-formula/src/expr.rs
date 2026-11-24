//! The expression tree: one node per Polish node (calc-encoding.md 3.1), so
//! a decoded Polish stream encodes back to the same bytes. Arguments are
//! always held in source order; the codec reverses the forms FileMaker
//! stores last first.

use crate::catalog::{NArgs, Opcode};
use crate::number::Number;
use crate::operand::{Field, Key, PluginFunctionId, Symbol, Text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// `00`: an omitted argument (an If without else, an empty plug-in
    /// argument); evaluates as empty text.
    Empty,
    /// `10`: a number literal.
    Number(Number),
    /// `11`: read like a number; FileMaker never writes it.
    Boolean(Number),
    /// `13`: a text literal (U+E004 already replaced by a space, 5.6).
    Text(Text),
    /// `16`: a field's value.
    Field(Field),
    /// `17`: a field where a field reference is required (5.3).
    FieldRef(Field),
    /// `0b`: an unresolved name; never in a stored Polish stream.
    Symbol(Symbol),
    /// `18`: a Let/While variable or custom function parameter by slot (5.5).
    Local(u16),
    /// `19`: `[ a ; b ; ... ]`.
    List(Vec<Expr>),
    /// `1a`: a `$` or `$$` variable; the name includes the dollar signs.
    Variable(String),
    /// `9b`: `Get ( selector )`.
    Get(u8),
    /// Functions, operators and constants whose arguments need no special
    /// form: fixed arity (including If, SetPrecision, SetRecursion,
    /// EvaluationError, IsValid, IsValidExpression and the internal
    /// SetVariable `e3` and GetVarRep `e4`), no arguments, or counted.
    Call { op: Opcode, args: Vec<Expr> },
    /// `4c`: (test, result) pairs; a default result is a final pair whose
    /// test is the number 1.
    Case(Vec<(Expr, Expr)>),
    /// `4d`.
    Choose { test: Box<Expr>, results: Vec<Expr> },
    /// `df`: one value per binding slot (a `SetVariable` call for `$`
    /// variables and repeated names), then the body.
    Let {
        bindings: Vec<Expr>,
        body: Box<Expr>,
    },
    /// `f8 00`.
    While {
        initial: Vec<Expr>,
        condition: Box<Expr>,
        logic: Vec<Expr>,
        result: Box<Expr>,
    },
    /// `dc`: one or two arguments; `need_field_ref` is 1 when the result is
    /// used as a field reference (5.3).
    Evaluate {
        args: Vec<Expr>,
        need_field_ref: u16,
    },
    /// `dd`.
    GetField { arg: Box<Expr>, need_field_ref: u16 },
    /// `1b` call or `1c` tail call of a custom function (5.2).
    CustomCall {
        tail: bool,
        function: Key,
        args: Vec<Expr>,
    },
    /// `1d`: a plug-in or built-in plug-in (Design, mobile) function.
    PluginCall {
        function: PluginFunctionId,
        args: Vec<Expr>,
    },
}

/// How the Polish stream stores a `Call`'s arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallForm {
    /// `arity` arguments in source order.
    Fixed(u8),
    /// Two arguments, second first (SetPrecision, SetRecursion).
    Swapped,
    /// A u16 count, then the arguments last first.
    Counted,
}

/// The `Call` form of `op`, or `None` when `op` has a dedicated variant or
/// is not valid in Polish code.
pub(crate) fn call_form(op: Opcode) -> Option<CallForm> {
    if op == Opcode::SET_PRECISION || op == Opcode::SET_RECURSION {
        return Some(CallForm::Swapped);
    }
    let info = op.info()?;
    match info.nargs {
        _ if (2300..2900).contains(&op.0) => Some(CallForm::Counted),
        NArgs::VarArgs if op != Opcode::PLUGIN_CALL => Some(CallForm::Counted),
        NArgs::NoArgs if op.0 >= 0x1e => Some(CallForm::Fixed(0)),
        NArgs::Unary | NArgs::Binary | NArgs::Trinary | NArgs::Quad => {
            Some(CallForm::Fixed(info.arity))
        }
        NArgs::UnaryField | NArgs::BinaryField => Some(CallForm::Fixed(info.arity)),
        NArgs::Special
            if matches!(op, Opcode::IF | Opcode::SET_VARIABLE | Opcode::GET_VAR_REP)
                || matches!(op.0, 0x4a | 0x4f | 0xde) =>
        {
            Some(CallForm::Fixed(info.arity))
        }
        _ => None,
    }
}

impl Expr {
    /// A [`Expr::Call`] node.
    pub fn call(op: Opcode, args: Vec<Expr>) -> Self {
        Self::Call { op, args }
    }

    /// The number of nodes on the longest path from this node down (a leaf
    /// is 1), counted without recursion.
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(self, 1)];
        while let Some((expr, depth)) = stack.pop() {
            deepest = deepest.max(depth);
            expr.for_each_child(|c| stack.push((c, depth + 1)));
        }
        deepest
    }

    /// Whether evaluating this may read a `$` or `$$` variable, in any
    /// branch: it names one (an assignment included), or reaches code not in
    /// this tree that might (a custom function's body, `Evaluate`'s text,
    /// `GetField`'s field, a plug-in, `ExecuteSQL`'s query). The fields it
    /// references are the caller's to check. Checked without recursion.
    pub fn may_read_variables(&self) -> bool {
        let (itself, functions) = self.variable_reads();
        itself || !functions.is_empty()
    }

    /// [`Self::may_read_variables`] in two parts: whether this tree itself
    /// may read a variable (it names one, or calls `Evaluate`, `GetField`, a
    /// plug-in or `ExecuteSQL`), and the custom functions it calls, each
    /// once, whose bodies a caller that can resolve them checks the same way.
    pub fn variable_reads(&self) -> (bool, Vec<Key>) {
        // Each call once, by a set: a formula of many calls stays linear.
        let mut functions = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![self];
        while let Some(expr) = stack.pop() {
            match expr {
                Expr::Variable(_)
                | Expr::Evaluate { .. }
                | Expr::GetField { .. }
                | Expr::PluginCall { .. } => return (true, functions),
                Expr::Call { op, .. }
                    if op.name().is_some_and(|name| {
                        name.eq_ignore_ascii_case("ExecuteSQL")
                            || name.eq_ignore_ascii_case("ExecuteSQLe")
                    }) =>
                {
                    return (true, functions)
                }
                Expr::CustomCall { function, .. } => {
                    if seen.insert(function) {
                        functions.push(function.clone());
                    }
                    expr.for_each_child(|c| stack.push(c));
                }
                _ => expr.for_each_child(|c| stack.push(c)),
            }
        }
        (false, functions)
    }

    /// Whether evaluating this may read `Self`, in any branch: it calls the
    /// `Self` function, or reaches code not in this tree (a custom
    /// function's body, `Evaluate`'s text). Checked without recursion.
    pub fn may_read_self(&self) -> bool {
        let mut stack = vec![self];
        while let Some(expr) = stack.pop() {
            match expr {
                Expr::Call { op, .. } if *op == Opcode::SELF => return true,
                Expr::CustomCall { .. } | Expr::Evaluate { .. } => return true,
                _ => expr.for_each_child(|c| stack.push(c)),
            }
        }
        false
    }

    /// Call `f` with each of the node's direct operands, so a caller can
    /// walk a formula with a stack of its own rather than by recursion.
    pub fn for_each_child<'a>(&'a self, mut f: impl FnMut(&'a Expr)) {
        match self {
            Expr::List(items)
            | Expr::Call { args: items, .. }
            | Expr::Evaluate { args: items, .. }
            | Expr::CustomCall { args: items, .. }
            | Expr::PluginCall { args: items, .. } => items.iter().for_each(f),
            Expr::Case(pairs) => {
                for (test, result) in pairs {
                    f(test);
                    f(result);
                }
            }
            Expr::Choose { test, results } => {
                f(test);
                results.iter().for_each(f);
            }
            Expr::Let { bindings, body } => {
                bindings.iter().for_each(&mut f);
                f(body);
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => {
                initial.iter().for_each(&mut f);
                f(condition);
                logic.iter().for_each(&mut f);
                f(result);
            }
            Expr::GetField { arg, .. } => f(arg),
            Expr::Empty
            | Expr::Number(_)
            | Expr::Boolean(_)
            | Expr::Text(_)
            | Expr::Field(_)
            | Expr::FieldRef(_)
            | Expr::Symbol(_)
            | Expr::Local(_)
            | Expr::Variable(_)
            | Expr::Get(_) => {}
        }
    }

    /// The fields the expression references directly, each once, in order
    /// (not those inside `Evaluate` or `GetField` text, nor a custom
    /// function's body).
    pub fn field_references(&self) -> Vec<Field> {
        let mut out = Vec::new();
        self.collect_fields(&mut out);
        out
    }

    fn collect_fields(&self, out: &mut Vec<Field>) {
        match self {
            Expr::Field(f) | Expr::FieldRef(f) => {
                if !out.contains(f) {
                    out.push(f.clone());
                }
            }
            Expr::List(items)
            | Expr::Call { args: items, .. }
            | Expr::Evaluate { args: items, .. } => {
                items.iter().for_each(|i| i.collect_fields(out))
            }
            Expr::CustomCall { args, .. } | Expr::PluginCall { args, .. } => {
                args.iter().for_each(|i| i.collect_fields(out))
            }
            Expr::Case(pairs) => pairs.iter().for_each(|(a, b)| {
                a.collect_fields(out);
                b.collect_fields(out);
            }),
            Expr::Choose { test, results } => {
                test.collect_fields(out);
                results.iter().for_each(|r| r.collect_fields(out));
            }
            Expr::Let { bindings, body } => {
                bindings.iter().for_each(|b| b.collect_fields(out));
                body.collect_fields(out);
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => {
                initial
                    .iter()
                    .chain(logic)
                    .for_each(|b| b.collect_fields(out));
                condition.collect_fields(out);
                result.collect_fields(out);
            }
            Expr::GetField { arg, .. } => arg.collect_fields(out),
            _ => {}
        }
    }
}
