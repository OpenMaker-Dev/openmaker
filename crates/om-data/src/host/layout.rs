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
    /// A button's calculated icon, the SVG text its "Calculated icon"
    /// formula gives (`LayoutObject::active_icon_formula`), shared by the
    /// states that keep it. Text longer than [`MAX_ICON_SVG`] is kept as
    /// none: empty, which draws the warning.
    pub icon_svg: Option<std::sync::Arc<str>>,
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
    state.icon_svg = object.active_icon_formula().map(|formula| {
        let svg = text(host, formula, context);
        match svg.len() > MAX_ICON_SVG {
            true => std::sync::Arc::from(""),
            false => std::sync::Arc::from(svg),
        }
    });
    state
}

/// The longest calculated icon kept and drawn, in bytes of SVG text: a
/// longer one is dropped as soon as it is computed, and draws the warning
/// an invalid icon does. OpenMaker's bound [I]: FileMaker Pro's is
/// unmeasured; icons are a few kilobytes.
pub const MAX_ICON_SVG: usize = 64 << 10;

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
const SYMBOLS: [(&str, u16, &str); 6] = [
    ("CurrentDate", 1002, "Get ( CurrentDate )"),
    ("CurrentTime", 1029, "Get ( CurrentTime )"),
    ("UserName", 1031, "Get ( UserName )"),
    ("AccountName", 1032, "Get ( AccountName )"),
    ("PageNumber", 1015, "Get ( PageNumber )"),
    ("RecordNumber", 1020, "Get ( RecordNumber )"),
];

/// Whether `inner` names symbol `name` whose selector's number is `op`:
/// the name in any case, or the number as FileMaker writes it (decimal
/// digits, no sign, no leading zero).
fn names_symbol(inner: &str, name: &str, op: u16) -> bool {
    name.eq_ignore_ascii_case(inner) || inner == op.to_string()
}

/// The `Get` selector of symbol `{{inner}}`. Another name or number is no
/// symbol.
fn symbol_selector(inner: &str) -> Option<&'static str> {
    symbol(inner).map(|(_, _, get)| *get)
}

/// The name of what symbol `{{inner}}` shows, as [`SYMBOLS`] spells it.
fn symbol_name(inner: &str) -> Option<&'static str> {
    symbol(inner).map(|(name, _, _)| *name)
}

fn symbol(inner: &str) -> Option<&'static (&'static str, u16, &'static str)> {
    SYMBOLS
        .iter()
        .find(|(name, op, _)| names_symbol(inner, name, *op))
}

/// A piece of layout text as [`merge_text`] reads it: text, or a merge
/// variable's or a symbol's inside (`raw`, the piece with its brackets).
enum Piece<'t> {
    Text(&'t str),
    Merge { inner: &'t str, raw: &'t str },
    Symbol { inner: &'t str, raw: &'t str },
}

/// `text` in [`Piece`]s: each `<<$` or `{{`, whichever comes first, up to
/// its closing `>>` or `}}`; an unclosed one is text. Read as they are
/// taken, so a reader that refuses a piece reads no further.
fn pieces(text: &str) -> Pieces<'_> {
    Pieces {
        text,
        at: Some(0),
        merge: None,
        symbol: None,
        held: None,
    }
}

struct Pieces<'t> {
    text: &'t str,
    /// Where the next piece starts; `None` at the end.
    at: Option<usize>,
    /// The next of each opener from `at`, kept until it is passed: text of
    /// many symbols and no merge variable is read once, not once a symbol.
    merge: Option<Option<usize>>,
    symbol: Option<Option<usize>>,
    /// The piece found after the text just given.
    held: Option<Piece<'t>>,
}

impl<'t> Iterator for Pieces<'t> {
    type Item = Piece<'t>;

    fn next(&mut self) -> Option<Piece<'t>> {
        if let Some(piece) = self.held.take() {
            return Some(piece);
        }
        let (text, at) = (self.text, self.at?);
        let next = |found: &mut Option<Option<usize>>, opener: &str| {
            if found.is_none_or(|next| next.is_some_and(|i| i < at)) {
                *found = Some(text[at..].find(opener).map(|i| i + at));
            }
            found.flatten()
        };
        let openers = [next(&mut self.merge, "<<$"), next(&mut self.symbol, "{{")];
        let Some(start) = openers.into_iter().flatten().min() else {
            self.at = None;
            return Some(Piece::Text(&text[at..]));
        };
        let tail = &text[start..];
        let is_merge = tail.starts_with("<<");
        let close = if is_merge { ">>" } else { "}}" };
        self.held = Some(match tail[2..].find(close).map(|i| i + 2) {
            None => {
                self.at = None;
                Piece::Text(tail)
            }
            Some(end) => {
                self.at = Some(start + end + 2);
                let (inner, raw) = (&tail[2..end], &tail[..end + 2]);
                match is_merge {
                    true => Piece::Merge { inner, raw },
                    false => Piece::Symbol { inner, raw },
                }
            }
        });
        Some(Piece::Text(&text[at..start]))
    }
}

/// What [`merge_text`] fills in, or would, for one piece of layout text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merged<'t> {
    /// A merge variable: `$name` or `$$name`, with an optional `[n]`.
    Variable(&'t str),
    /// A symbol, by the name of what it shows: `CurrentDate` for
    /// `{{currentdate}}` or `{{1002}}`.
    Symbol(&'static str),
    /// A `{{symbol}}` [`merge_text`] keeps as written: another name or
    /// number, `{{PageCount}}` among them. Its inside.
    Kept(&'t str),
    /// A `<<$...>>` [`merge_text`] keeps as written, since its inside is no
    /// variable reference it reads: a name with a space, say, which Set
    /// Variable accepts (script-runtime.md 13.6). Its inside.
    KeptVariable(&'t str),
}

/// The merge variables and symbols of layout text, read as [`merge_text`]
/// reads them: a symbol never inside a merge variable, an unclosed one
/// text. A `<<...>>` without the `$` is text too.
pub fn merged(text: &str) -> impl Iterator<Item = Merged<'_>> {
    pieces(text).filter_map(|piece| match piece {
        Piece::Text(_) => None,
        Piece::Merge { inner, .. } => Some(match is_variable_reference(inner) {
            true => Merged::Variable(inner),
            false => Merged::KeptVariable(inner),
        }),
        Piece::Symbol { inner, .. } => {
            Some(symbol_name(inner).map_or(Merged::Kept(inner), Merged::Symbol))
        }
    })
}

/// `text` with each symbol that `shown` gives a text for, by the name of
/// what it shows ([`Merged::Symbol`]), replaced by that text; everything
/// else is kept as written. Symbols are read as [`merge_text`] reads them,
/// so what is left fills in as it would have.
pub fn replace_symbols(text: &str, mut shown: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in pieces(text) {
        let raw = match piece {
            Piece::Text(text) => text,
            Piece::Merge { raw, .. } => raw,
            Piece::Symbol { inner, raw } => match symbol_name(inner).and_then(&mut shown) {
                Some(text) => {
                    out.push_str(&text);
                    continue;
                }
                None => raw,
            },
        };
        out.push_str(raw);
    }
    out
}

/// Whether layout text holds `{{PageNumber}}` or `{{PageCount}}`, by name
/// or by its selector's number (`{{1015}}`, `{{1125}}`; calc-values.md
/// "Layout calculations"), read as [`merge_text`] reads its symbols.
pub(super) fn has_page_symbol(text: &str) -> bool {
    pieces(text).any(|piece| match piece {
        Piece::Symbol { inner, .. } => {
            names_symbol(inner, "PageNumber", 1015) || names_symbol(inner, "PageCount", 1125)
        }
        _ => false,
    })
}

/// Layout text with its merge variables (`<<$$name>>`, `<<$name>>`, with
/// an optional `[repetition]`) and `{{symbols}}` replaced by their values
/// in `context`. Other text, including unresolved `<<...>>`, is kept.
pub fn merge_text(host: &mut dyn CalcHost, text: &str, context: &Context) -> String {
    // Unchecked, nothing fails: a failed value shows as `display` shows it.
    merge(host, text, context, false).unwrap_or_default()
}

/// [`merge_text`] for output that must not show a failed value, such as a
/// report: the error code of the first evaluation that failed, waits for
/// data (`pending_execution`) or is `?`, instead of the text; 0 when it
/// has none.
pub fn merge_text_checked(
    host: &mut dyn CalcHost,
    text: &str,
    context: &Context,
) -> Result<String, i32> {
    merge(host, text, context, true)
}

fn merge(
    host: &mut dyn CalcHost,
    text: &str,
    context: &Context,
    checked: bool,
) -> Result<String, i32> {
    let mut out = String::with_capacity(text.len());
    for piece in pieces(text) {
        let (formula, raw) = match piece {
            Piece::Text(text) => {
                out.push_str(text);
                continue;
            }
            Piece::Merge { inner, raw } => {
                (is_variable_reference(inner).then(|| inner.to_owned()), raw)
            }
            Piece::Symbol { inner, raw } => (symbol_selector(inner).map(str::to_owned), raw),
        };
        match formula {
            Some(formula) => {
                let evaluation = evaluate_text(&formula, host, context);
                if checked
                    && (evaluation.pending_execution
                        || evaluation.error.is_some()
                        || evaluation.value.is_invalid())
                {
                    return Err(evaluation.code());
                }
                out.push_str(&display(&evaluation.value, host));
            }
            None => out.push_str(raw),
        }
    }
    Ok(out)
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
    fn page_symbols_are_read_as_merge_text_reads_symbols() {
        for page in [
            "Page {{1015}}",
            "{{pagecount}} pages",
            "{{PageNumber}}",
            "{{1125}}",
        ] {
            assert!(has_page_symbol(page), "{page}");
        }
        // A number spelled otherwise, and a symbol inside an unclosed one,
        // are text, as merge_text keeps them.
        for text in ["{{01015}}", "{{x{{1015}}", "{{1015", "Page 1015"] {
            assert!(!has_page_symbol(text), "{text}");
        }
        // Reports read and number symbols as merge_text does: one inside a
        // merge variable is the variable's text, and numbering cannot make
        // a variable of it.
        let text = "<<$x{{PageNumber}}>> {{1015}} <<$$y[2]>> {{pagecount}} {{1002}} <<Name>>";
        assert_eq!(
            merged(text).collect::<Vec<_>>(),
            [
                Merged::KeptVariable("$x{{PageNumber}}"),
                Merged::Symbol("PageNumber"),
                Merged::Variable("$$y[2]"),
                Merged::Kept("pagecount"),
                Merged::Symbol("CurrentDate"),
            ]
        );
        assert_eq!(
            replace_symbols(text, |name| (name == "PageNumber").then(|| "2".into())),
            "<<$x{{PageNumber}}>> 2 <<$$y[2]>> {{pagecount}} {{1002}} <<Name>>"
        );
        // Symbols after one another, a variable after them, and openers
        // passed inside a piece are each read once, in order.
        let long = "{{1015}}".repeat(3) + "<<$$a>>{{ <<$$b>> }}<<$$c>>";
        assert_eq!(
            merged(&long).collect::<Vec<_>>(),
            [
                Merged::Symbol("PageNumber"),
                Merged::Symbol("PageNumber"),
                Merged::Symbol("PageNumber"),
                Merged::Variable("$$a"),
                Merged::Kept(" <<$$b>> "),
                Merged::Variable("$$c"),
            ]
        );
    }

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
