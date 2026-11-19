//! Step text: every step as FileMaker's Script Workspace shows it, such as
//! `Go to Object [ Object Name: Get ( ScriptParameter ) ]` (script-steps.md,
//! "Implementation notes (step text)").
//!
//! The step's options follow its name in brackets, separated by ` ; `, with
//! FileMaker's labels and order (the Claris script step reference and its
//! examples). Calculations are printed from their formatted stream by
//! `om-formula`'s printer, fields, layouts, scripts and table occurrences by
//! their current names ([`StepNames`]); a disabled step starts with `// `,
//! a comment with `# `, and steps inside If, Loop and Open Transaction
//! blocks are indented ([`script_lines`]). Each option's place in the text
//! is kept ([`OptionSpan`]), so an editor can tell which option was clicked.

mod expr;
mod parse;
mod steps;
mod text;
mod xml;

pub use parse::{compile, Issue, Part};
pub use text::{
    canonical, display_reading, escape_comment, failed_comment, numbered_records, parse_record,
    parse_steps, paste_limits, step_text, step_text_fidelity, steps_text, unescape_comment,
    Fidelity, Origin, Pasted, MAX_PASTE_BYTES, MAX_PASTE_STEPS, TEXT_INDENT,
};
pub use xml::{parse_xml_steps, MAX_XML_ATTRIBUTES, MAX_XML_DEPTH};

use crate::flow;
use crate::key::FieldSpec;
use crate::step::{Calculation, ScriptStep};
use om_formula::{Names, NoNames};
use std::ops::Range;

/// What a missing script, layout or table reference shows.
pub const UNKNOWN: &str = "<unknown>";

/// The names a step's text needs: fields and table occurrences (through
/// [`StepNames::calc_names`], which calculations print with too), layouts,
/// scripts, base tables, data sources and menu sets. Each lookup is by the
/// id FileMaker shows; the defaults know no names.
pub trait StepNames {
    /// The names calculations and field references print with.
    fn calc_names(&self) -> &dyn Names;
    /// Whether the file writes decimals with a comma (`1,5`).
    fn decimal_comma(&self) -> bool {
        false
    }
    /// A layout's name.
    fn layout_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// The table occurrence a layout shows records from.
    fn layout_occurrence_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// A script's name (by its serial id).
    fn script_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// A base table's name (by its serial id).
    fn table_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// An external data source's name.
    fn data_source_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// A custom menu set's name.
    fn menu_set_name(&self, _id: u64) -> Option<&str> {
        None
    }
    /// The layout named `name`; when `occurrence` is given, of that table
    /// occurrence (the text form shows layouts as `“Name” (Occurrence)`).
    fn find_layout(&self, _name: &str, _occurrence: Option<&str>) -> Lookup {
        Lookup::Missing
    }
    /// The script named `name`, by its serial id.
    fn find_script(&self, _name: &str) -> Lookup {
        Lookup::Missing
    }
    /// The base table named `name`.
    fn find_table(&self, _name: &str) -> Lookup {
        Lookup::Missing
    }
    /// The value list named `name`, by the id of its key.
    fn find_value_list(&self, _name: &str) -> Lookup {
        Lookup::Missing
    }
    /// The table occurrence named `name`; by default the calculation
    /// names', which keep one of each name.
    fn find_occurrence(&self, name: &str) -> Lookup {
        self.calc_names().find_occurrence(name).into()
    }
    /// The field named `name` of the table `occurrence` shows; by default
    /// the calculation names'.
    fn find_field(&self, occurrence: u64, name: &str) -> Lookup {
        self.calc_names().find_field(occurrence, name).into()
    }
}

/// What a lookup by name finds in the open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Nothing has the name.
    Missing,
    /// One object has it.
    One(u64),
    /// Several share it, so the name does not say which.
    Ambiguous,
}

impl Lookup {
    /// The objects among `items` (id, name) named `name`: those with the
    /// name exactly, else those with it ignoring case, as FileMaker matches
    /// names.
    pub fn by_name<'a>(items: impl IntoIterator<Item = (u64, &'a str)>, name: &str) -> Lookup {
        let items: Vec<(u64, &str)> = items.into_iter().collect();
        let exact: Vec<u64> = items
            .iter()
            .filter(|(_, n)| *n == name)
            .map(|(id, _)| *id)
            .collect();
        if !exact.is_empty() {
            return Lookup::of(exact);
        }
        let folded = name.to_lowercase();
        Lookup::of(
            items
                .iter()
                .filter(|(_, n)| n.to_lowercase() == folded)
                .map(|(id, _)| *id)
                .collect(),
        )
    }

    /// One id, none, or several (an id found twice counts once).
    pub fn of(mut ids: Vec<u64>) -> Lookup {
        ids.sort_unstable();
        ids.dedup();
        match ids[..] {
            [] => Lookup::Missing,
            [id] => Lookup::One(id),
            _ => Lookup::Ambiguous,
        }
    }

    /// The one object found.
    pub fn one(self) -> Option<u64> {
        match self {
            Lookup::One(id) => Some(id),
            _ => None,
        }
    }

    /// The object found, or why there is none: `what` names the kind and
    /// the name (`the layout “Detail”`).
    pub fn require(self, what: &str) -> Result<u64, String> {
        match self {
            Lookup::One(id) => Ok(id),
            Lookup::Missing => Err(format!("{} is not in this file", capitalized(what))),
            Lookup::Ambiguous => Err(format!(
                "{} is ambiguous: several in this file have that name",
                capitalized(what)
            )),
        }
    }
}

impl From<Option<u64>> for Lookup {
    fn from(id: Option<u64>) -> Self {
        id.map_or(Lookup::Missing, Lookup::One)
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// No names: references show as missing.
pub struct NoStepNames;

impl StepNames for NoStepNames {
    fn calc_names(&self) -> &dyn Names {
        &NoNames
    }
}

/// What a line of the step list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Step,
    /// A `# (comment)` step: FileMaker shows its text in bold after `# `,
    /// and an empty comment as a blank line.
    Comment,
}

/// One option in a step's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionSpan {
    /// Byte range in [`StepLine::text`].
    pub range: Range<usize>,
    /// The schema parameter it shows (index into `step.schema().params`),
    /// `None` for fixed text.
    pub parameter: Option<usize>,
    /// The calculation slot shown by this option. One parameter can have
    /// several slots, such as a window's Height, Width, Top and Left.
    /// Editors must keep this identity rather than choosing its first slot.
    pub calculation_slot: Option<u16>,
}

/// A step as the Script Workspace shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepLine {
    /// Block nesting level: steps inside If, Loop and Open Transaction
    /// blocks are indented one level per block.
    pub depth: usize,
    pub enabled: bool,
    pub kind: LineKind,
    /// The text, without indentation; disabled steps start with `// `.
    pub text: String,
    /// Each option's place in `text`, in order.
    pub options: Vec<OptionSpan>,
}

impl StepLine {
    /// The option at byte `at` of the text, if any.
    pub fn option_at(&self, at: usize) -> Option<&OptionSpan> {
        self.options.iter().find(|o| o.range.contains(&at))
    }
}

/// The prefix of a disabled step.
pub const DISABLED: &str = "// ";
/// The prefix of a comment.
pub const COMMENT: &str = "# ";
/// One indentation level in [`script_text`].
pub const INDENT: &str = "    ";

/// What a step's line is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// The Script Workspace's display.
    Display,
    /// The text form (copy and paste), which must read back: a comment's
    /// returns and Insert Text's quote characters are escaped there only,
    /// as FileMaker's display of them was not observed.
    Text,
}

/// A step's line, at depth 0.
pub fn step_line(step: &ScriptStep, names: &dyn StepNames) -> StepLine {
    line(step, names, Form::Display)
}

fn line(step: &ScriptStep, names: &dyn StepNames, form: Form) -> StepLine {
    let (kind, body, mut options) = steps::text(step, names, form);
    let mut text = String::new();
    if !step.is_enabled() {
        text.push_str(DISABLED);
    }
    let shift = text.len();
    text.push_str(&body);
    for option in &mut options {
        option.range = option.range.start + shift..option.range.end + shift;
    }
    StepLine {
        depth: 0,
        enabled: step.is_enabled(),
        kind,
        text,
        options,
    }
}

/// Every step's line, indented by the block structure of the enabled steps
/// (a disabled If opens no block, as it is not one when the script runs).
pub fn script_lines(steps: &[ScriptStep], names: &dyn StepNames) -> Vec<StepLine> {
    let mut depth = 0usize;
    steps
        .iter()
        .map(|step| {
            let mut line = step_line(step, names);
            let (shown, after) = if step.is_enabled() {
                match step.id() {
                    flow::IF | flow::LOOP | flow::OPEN_TRANSACTION => (depth, depth + 1),
                    flow::ELSE | flow::ELSE_IF => (depth.saturating_sub(1), depth),
                    flow::END_IF | flow::END_LOOP | flow::COMMIT_TRANSACTION => {
                        let d = depth.saturating_sub(1);
                        (d, d)
                    }
                    _ => (depth, depth),
                }
            } else {
                (depth, depth)
            };
            line.depth = shown;
            depth = after;
            line
        })
        .collect()
}

/// The script as text, one step per line, indented by [`INDENT`].
pub fn script_text(steps: &[ScriptStep], names: &dyn StepNames) -> String {
    script_lines(steps, names)
        .iter()
        .map(|line| format!("{}{}", INDENT.repeat(line.depth), line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A stored calculation as FileMaker shows it: its formatted stream
/// printed with the file's names; a formula whose text was stripped is
/// rebuilt from its Polish stream (OpenMaker's form, script-steps.md).
pub fn calculation_text(calc: &Calculation, names: &dyn StepNames) -> String {
    let ctx = om_formula::Context {
        decimal_comma: names.decimal_comma(),
        ..om_formula::Context::new(names.calc_names())
    };
    if let Some(tokens) = calc
        .formatted
        .as_deref()
        .and_then(|bytes| om_formula::formatted::decode(bytes).ok())
    {
        return om_formula::print(&tokens, &ctx);
    }
    match calc
        .polish
        .as_deref()
        .map(om_formula::polish::decode)
        .transpose()
    {
        Ok(Some(expr)) => expr::text(&expr, &ctx),
        _ => UNKNOWN.to_owned(),
    }
}

/// A field reference as `Table::Field`, with `[n]` for a repetition past
/// the first.
pub fn field_text(spec: &FieldSpec, names: &dyn StepNames) -> String {
    let calc = names.calc_names();
    let occurrence = spec.table().id();
    let table = occurrence
        .and_then(|o| calc.occurrence_name(o))
        .unwrap_or(om_formula::TABLE_MISSING);
    let field = occurrence
        .zip(spec.field().id())
        .and_then(|(o, f)| calc.field_name(o, f))
        .unwrap_or(om_formula::FIELD_MISSING);
    let mut text = format!("{table}::{field}");
    if spec.repetition() > 1 {
        text.push_str(&format!("[{}]", spec.repetition()));
    }
    text
}

/// A name in FileMaker's typographic quotes, as layouts, scripts, tables
/// and file paths show.
pub fn quoted(name: &str) -> String {
    format!("“{name}”")
}

#[cfg(test)]
mod tests;
