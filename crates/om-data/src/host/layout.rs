//! Layout calculations (calc-encoding.md 8, layout-model.md 5.5): hide
//! conditions, conditional formatting, tooltips, placeholder text, button
//! labels, script parameters and merge variables, evaluated for one object
//! on one record.
//!
//! The context is the record the object shows: the window's current record
//! in Form view, the row's record in List and Table view, the portal row in
//! a portal (`Context::portal`). `Self` is the object's content: a field
//! object's value, a text or button object's text.

use om_calc::eval::{display, evaluate_formula, evaluate_text, CalcHost, Context, Evaluation};
use om_calc::{Ctx, Value};
use om_formula::Formula;
use om_layout::LayoutObject;

/// What an object's calculations give on one record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectState {
    /// "Hide object when" holds.
    pub hidden: bool,
    /// The conditional formats whose condition holds, as indexes into
    /// `calcs.conditions`, in stored order.
    pub conditions: Vec<usize>,
    pub tooltip: Option<String>,
    pub placeholder: Option<String>,
    /// A calculated label (buttons, popover buttons, panels).
    pub label: Option<String>,
}

/// Evaluate a layout formula in `context`. A formula whose stored context
/// is an occurrence keeps the window's (layout calculations store
/// occurrence 0, calc-encoding.md 1).
pub fn evaluate(host: &mut dyn CalcHost, formula: &Formula, context: &Context) -> Evaluation {
    evaluate_formula(formula, host, context)
}

/// A formula's result as text, as a label or tooltip shows it.
pub fn text(host: &mut dyn CalcHost, formula: &Formula, context: &Context) -> String {
    let result = evaluate(host, formula, context);
    display(&result.value, host)
}

/// Whether a condition holds: true when its value is (calc-values.md 2.9);
/// a `?` result is true, as `If` takes it.
pub fn holds(host: &mut dyn CalcHost, formula: &Formula, context: &Context) -> bool {
    let result = evaluate(host, formula, context);
    let locale = host.locale().clone();
    let cx = Ctx::new(&locale, host.today());
    result.value.is_true(&cx)
}

/// Every calculation of `object` on the record of `context`, with `Self`
/// set to `self_value`.
pub fn object_state(
    host: &mut dyn CalcHost,
    object: &LayoutObject,
    context: &Context,
    self_value: Value,
) -> ObjectState {
    let context = context.clone().with_self(self_value);
    if let Some(hide) = &object.calcs.hide {
        if holds(host, hide, &context) {
            return ObjectState {
                hidden: true,
                ..ObjectState::default()
            };
        }
    }
    display_state(host, object, &context)
}

/// The calculations of a shown object other than its hide condition, on the
/// record of `context`, whose `Self` the caller has set.
pub fn display_state(
    host: &mut dyn CalcHost,
    object: &LayoutObject,
    context: &Context,
) -> ObjectState {
    let calcs = &object.calcs;
    let mut state = ObjectState {
        conditions: conditions(host, object, context),
        ..ObjectState::default()
    };
    state.tooltip = calcs.tooltip.as_ref().map(|f| text(host, f, context));
    state.placeholder = calcs.placeholder.as_ref().map(|f| text(host, f, context));
    state.label = calcs.label.as_ref().map(|f| text(host, f, context));
    state
}

/// The conditional formats of `object` whose condition holds on the record
/// of `context`, whose `Self` the caller has set: indexes into
/// `calcs.conditions`, in stored order. An entry without a formula never
/// holds.
pub fn conditions(host: &mut dyn CalcHost, object: &LayoutObject, context: &Context) -> Vec<usize> {
    let mut held = Vec::new();
    for (i, condition) in object.calcs.conditions.iter().enumerate() {
        if let Some(formula) = &condition.formula {
            if holds(host, formula, context) {
                held.push(i);
            }
        }
    }
    held
}

/// A button's script parameter, `None` when it stores no calculation.
pub fn script_parameter(
    host: &mut dyn CalcHost,
    object: &LayoutObject,
    context: &Context,
) -> Option<Value> {
    let formula = object.calcs.script_parameter.as_ref()?;
    Some(evaluate(host, formula, context).value)
}

/// The `{{symbol}}`s of layout text, the number of the `Get` selector each
/// shows (1000 + its index, calc-functions.md 2), which a text may hold in
/// the name's place (`{{1002}}`, calc-values.md "Layout calculations"),
/// and the selector.
const SYMBOLS: [(&str, u16, &str); 5] = [
    ("CurrentDate", 1002, "Get ( CurrentDate )"),
    ("CurrentTime", 1029, "Get ( CurrentTime )"),
    ("UserName", 1031, "Get ( UserName )"),
    ("PageNumber", 1015, "Get ( PageNumber )"),
    ("RecordNumber", 1020, "Get ( RecordNumber )"),
];

/// The `Get` selector of symbol `{{inner}}`: named in any case, or by its
/// selector's number. Another number is no symbol.
fn symbol_selector(inner: &str) -> Option<&'static str> {
    let number = Some(inner)
        .filter(|inner| !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|inner| inner.parse::<u16>().ok());
    SYMBOLS
        .iter()
        .find(|(name, op, _)| name.eq_ignore_ascii_case(inner) || number == Some(*op))
        .map(|(_, _, get)| *get)
}

/// Layout text with its merge variables (`<<$$name>>`, `<<$name>>`, with
/// an optional `[repetition]`) and `{{symbols}}` replaced by their values
/// in `context`. Other text, including unresolved `<<...>>`, is kept.
pub fn merge_text(host: &mut dyn CalcHost, text: &str, context: &Context) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let merge = rest.find("<<$");
        let symbol = rest.find("{{");
        let Some(start) = [merge, symbol].into_iter().flatten().min() else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let (open, close) = if tail.starts_with("<<") {
            ("<<", ">>")
        } else {
            ("{{", "}}")
        };
        let Some(end) = tail[2..].find(close).map(|i| i + 2) else {
            out.push_str(tail);
            return out;
        };
        let inner = &tail[2..end];
        let formula = if open == "<<" {
            is_variable_reference(inner).then(|| inner.to_owned())
        } else {
            symbol_selector(inner).map(str::to_owned)
        };
        match formula {
            Some(formula) => {
                let value = evaluate_text(&formula, host, context).value;
                out.push_str(&display(&value, host));
            }
            None => out.push_str(&tail[..end + 2]),
        }
        rest = &tail[end + 2..];
    }
}

/// `$name`, `$$name` or either with `[n]`: what a merge variable may hold.
fn is_variable_reference(text: &str) -> bool {
    let name = text.trim_start_matches('$');
    let dollars = text.len() - name.len();
    if !(1..=2).contains(&dollars) || name.is_empty() {
        return false;
    }
    let (name, index) = match name.split_once('[') {
        Some((n, i)) => (n, Some(i)),
        None => (name, None),
    };
    let name_ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.'));
    let index_ok = index.map_or(true, |i| {
        i.strip_suffix(']')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    });
    name_ok && index_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_references_are_dollar_names_with_an_optional_repetition() {
        for ok in ["$$x", "$x", "$$total_1", "$$a.b", "$$x[2]"] {
            assert!(is_variable_reference(ok), "{ok}");
        }
        for bad in ["$$$x", "x", "$$", "$$x y", "$$x[]", "$$x[a]", "$$x[2"] {
            assert!(!is_variable_reference(bad), "{bad}");
        }
    }
}
