//! A formula whose formatted stream was stripped, shown from its Polish
//! tree. How FileMaker shows such a formula is not known
//! (calc-encoding.md 10, item 7); this is OpenMaker's form: FileMaker's
//! spacing (`Get ( FoundCount )`, `a + b`), parentheses only where the
//! operator precedence needs them, and Let variables, whose names the
//! Polish stream does not keep, as `_v0`, `_v1`, ...

use om_formula::catalog::{self, Kind};
use om_formula::{Context, Expr, Number, Opcode, FIELD_MISSING, FUNCTION_MISSING, TABLE_MISSING};

/// The formula's text.
pub(super) fn text(expr: &Expr, ctx: &Context<'_>) -> String {
    let mut out = String::new();
    write(expr, ctx, &mut out);
    out
}

/// The binding strength of an operator (higher binds tighter).
fn precedence(op: Opcode) -> Option<u8> {
    Some(match op {
        Opcode::OR | Opcode::XOR => 1,
        Opcode::AND => 2,
        Opcode::NOT => 3,
        Opcode(0x41..=0x49) => 4,
        Opcode::CONCAT => 5,
        Opcode::ADD | Opcode::SUBTRACT => 6,
        Opcode::MULTIPLY | Opcode::DIVIDE => 7,
        Opcode::POWER => 8,
        Opcode::NEGATE => 9,
        _ => return None,
    })
}

fn operator(expr: &Expr) -> Option<u8> {
    match expr {
        Expr::Call { op, .. } => precedence(*op),
        _ => None,
    }
}

/// An operand, in parentheses when it binds looser than `min`.
fn operand(expr: &Expr, min: u8, ctx: &Context<'_>, out: &mut String) {
    let wrap = operator(expr).is_some_and(|p| p < min);
    if wrap {
        out.push_str("( ");
    }
    write(expr, ctx, out);
    if wrap {
        out.push_str(" )");
    }
}

fn call(name: &str, args: &[Expr], ctx: &Context<'_>, out: &mut String) {
    out.push_str(name);
    if args.is_empty() {
        return;
    }
    out.push_str(" ( ");
    list(args, ctx, out);
    out.push_str(" )");
}

fn list(args: &[Expr], ctx: &Context<'_>, out: &mut String) {
    for (n, arg) in args.iter().enumerate() {
        if n > 0 {
            out.push_str(" ; ");
        }
        write(arg, ctx, out);
    }
}

fn local(slot: u16) -> String {
    format!("_v{slot}")
}

fn write(expr: &Expr, ctx: &Context<'_>, out: &mut String) {
    let names = ctx.names;
    match expr {
        Expr::Empty => {}
        Expr::Number(n) | Expr::Boolean(n) => match n {
            Number::Decimal(d) => {
                let text = d.display(0);
                out.push_str(&if ctx.decimal_comma {
                    text.replace('.', ",")
                } else {
                    text
                });
            }
            Number::NotANumber => out.push('?'),
            Number::Empty => {}
        },
        Expr::Text(t) => literal(&t.value, out),
        Expr::Field(f) | Expr::FieldRef(f) => {
            let occurrence = f.occurrence.id();
            let table = occurrence.and_then(|o| names.occurrence_name(o));
            let field = occurrence
                .zip(f.field.id())
                .and_then(|(o, id)| names.field_name(o, id));
            if f.flags & om_formula::Field::TABLE_PRESENT != 0 || field.is_none() {
                out.push_str(table.unwrap_or(TABLE_MISSING));
                out.push_str("::");
            }
            out.push_str(field.unwrap_or(FIELD_MISSING));
        }
        Expr::Symbol(s) => out.push_str(&s.name.value),
        Expr::Local(slot) => out.push_str(&local(*slot)),
        Expr::List(items) => {
            out.push_str("[ ");
            list(items, ctx, out);
            out.push_str(" ]");
        }
        Expr::Variable(name) => out.push_str(name),
        Expr::Get(selector) => {
            out.push_str("Get ( ");
            out.push_str(catalog::get_selector_name(*selector).unwrap_or(FUNCTION_MISSING));
            out.push_str(" )");
        }
        Expr::Call { op, args } => match (precedence(*op), args.as_slice()) {
            (Some(p), [only]) => {
                out.push_str(op.name().unwrap_or(FUNCTION_MISSING));
                if *op == Opcode::NOT {
                    out.push(' ');
                }
                operand(only, p, ctx, out);
            }
            (Some(p), [left, right]) => {
                operand(left, p, ctx, out);
                out.push(' ');
                out.push_str(op.name().unwrap_or(FUNCTION_MISSING));
                out.push(' ');
                operand(right, p + 1, ctx, out);
            }
            _ if *op == Opcode::GET_VAR_REP => {
                if let [repetition, variable] = args.as_slice() {
                    write(variable, ctx, out);
                    out.push('[');
                    write(repetition, ctx, out);
                    out.push(']');
                }
            }
            _ if *op == Opcode::SET_VARIABLE => {
                // A Let binding of a `$` variable: value, repetition, target.
                if let [value, _, target] = args.as_slice() {
                    write(target, ctx, out);
                    out.push_str(" = ");
                    write(value, ctx, out);
                }
            }
            _ => {
                let name = op
                    .info()
                    .filter(|i| matches!(i.kind, Kind::Function | Kind::Constant | Kind::Operator))
                    .map(|i| i.name)
                    .filter(|n| !n.is_empty())
                    .unwrap_or(FUNCTION_MISSING);
                call(name, args, ctx, out);
            }
        },
        Expr::Case(pairs) => {
            let mut args = Vec::new();
            for (n, (test, result)) in pairs.iter().enumerate() {
                let default = n + 1 == pairs.len() && pairs.len() > 1 && is_one(test);
                if !default {
                    args.push(test.clone());
                }
                args.push(result.clone());
            }
            call("Case", &args, ctx, out);
        }
        Expr::Choose { test, results } => {
            let mut args = vec![(**test).clone()];
            args.extend(results.iter().cloned());
            call("Choose", &args, ctx, out);
        }
        Expr::Let { bindings, body } => {
            out.push_str("Let ( [ ");
            for (n, binding) in bindings.iter().enumerate() {
                if n > 0 {
                    out.push_str(" ; ");
                }
                if matches!(binding, Expr::Call { op, .. } if *op == Opcode::SET_VARIABLE) {
                    write(binding, ctx, out);
                } else {
                    out.push_str(&local(n as u16));
                    out.push_str(" = ");
                    write(binding, ctx, out);
                }
            }
            out.push_str(" ] ; ");
            write(body, ctx, out);
            out.push_str(" )");
        }
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => {
            out.push_str("While ( [ ");
            list(initial, ctx, out);
            out.push_str(" ] ; ");
            write(condition, ctx, out);
            out.push_str(" ; [ ");
            list(logic, ctx, out);
            out.push_str(" ] ; ");
            write(result, ctx, out);
            out.push_str(" )");
        }
        Expr::Evaluate { args, .. } => call("Evaluate", args, ctx, out),
        Expr::GetField { arg, .. } => call("GetField", std::slice::from_ref(&**arg), ctx, out),
        Expr::CustomCall { function, args, .. } => {
            let name = function
                .id()
                .and_then(|id| names.custom_function(id))
                .map(|f| f.name)
                .unwrap_or(FUNCTION_MISSING);
            call(name, args, ctx, out);
        }
        Expr::PluginCall { function, args } => {
            let name = catalog::builtin(*function)
                .map(|b| b.name)
                .or_else(|| names.plugin_function(*function).map(|f| f.name))
                .unwrap_or(FUNCTION_MISSING);
            call(name, args, ctx, out);
        }
    }
}

fn is_one(expr: &Expr) -> bool {
    matches!(expr, Expr::Number(Number::Decimal(d)) if d.display(0) == "1")
}

/// A text literal as FileMaker quotes it: quotes and backslashes escaped,
/// a return shown as `¶` and a `¶` as `\¶`.
fn literal(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\r' => out.push('¶'),
            '¶' => out.push_str("\\¶"),
            c => out.push(c),
        }
    }
    out.push('"');
}
