//! Script steps as text and back: the Script Workspace's copy and paste
//! (script-steps.md, "Text form of steps").
//!
//! A step's text is its display line (`step_line`, with a comment's returns
//! and Insert Text's quote characters escaped), indented with one tab per
//! block level. When that line alone would not give back the step
//! exactly, `Stored …` options follow its displayed options and carry what
//! the display leaves out (stale bytes, opaque specs, unmapped flags), so
//! the text is lossless: [`parse_steps`] of [`steps_text`] gives back the
//! same steps, as [`canonical`] compares them.

use super::parse::{Issue, Parser, Part};
use super::xml::parse_xml_steps;
use super::{script_lines, StepNames, DISABLED};
use crate::key::FieldSpec;
use crate::step::{flag, ScriptStep, StepData};
use om_hbam::{KeyTree, OwnedNode, SlotReader};
use std::collections::{BTreeMap, HashMap};

/// One block level of the text form.
pub const TEXT_INDENT: &str = "\t";

/// The comment step's id.
const COMMENT_STEP: u16 = 89;

/// A pasted line: a step, or text that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pasted {
    Step(ScriptStep),
    Failed {
        /// The text as pasted (its lines joined by returns).
        text: String,
        reason: String,
    },
}

/// Escape a comment's text for one line: a return is `¶`, a `¶` is `\¶`,
/// and a backslash before either (or another backslash) is doubled.
pub fn escape_comment(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => out.push('¶'),
            '¶' => out.push_str("\\¶"),
            '\\' if matches!(chars.peek(), Some('¶' | '\\' | '\r' | '\n')) => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

/// The inverse of [`escape_comment`].
pub fn unescape_comment(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if matches!(chars.peek(), Some('¶' | '\\')) => {
                out.push(chars.next().expect("peeked"));
            }
            '¶' => out.push('\r'),
            c => out.push(c),
        }
    }
    out
}

/// Tracks literals, comments and nesting while text is read.
#[derive(Clone, Debug, Default)]
pub(super) struct Scanner {
    depth: i32,
    /// In a `"` literal (closed by any quote character).
    literal: bool,
    /// In a `“…”` name.
    name: bool,
    block_comment: bool,
    line_comment: bool,
    escape: bool,
    previous: Option<char>,
}

/// The characters that close a calculation's text literal.
fn is_quote(c: char) -> bool {
    matches!(c, '"' | '“' | '”' | '„' | '‟' | '＂')
}

impl Scanner {
    /// Outside literals, names and comments, at nesting 0.
    pub fn at_top(&self) -> bool {
        self.depth <= 0 && !self.inside()
    }

    /// A scanner inside brackets, where `//` and `/*` start comments.
    pub fn bracketed() -> Self {
        Self {
            depth: 1,
            ..Self::default()
        }
    }

    /// Outside literals, names and comments, at nesting `depth`.
    pub fn at(&self, depth: i32) -> bool {
        self.depth == depth && !self.inside()
    }

    /// In a `//` comment, which only a line break ends.
    pub fn in_line_comment(&self) -> bool {
        self.line_comment
    }

    fn inside(&self) -> bool {
        self.literal || self.name || self.block_comment || self.line_comment
    }

    pub fn feed(&mut self, c: char) {
        let previous = self.previous.replace(c);
        if self.escape {
            self.escape = false;
            return;
        }
        if self.literal {
            match c {
                '\\' => self.escape = true,
                c if is_quote(c) => self.literal = false,
                _ => {}
            }
            return;
        }
        if self.name {
            if c == '”' {
                self.name = false;
            }
            return;
        }
        if self.block_comment {
            if previous == Some('*') && c == '/' {
                self.block_comment = false;
                self.previous = None;
            }
            return;
        }
        if self.line_comment {
            if c == '\n' || c == '\r' {
                self.line_comment = false;
            }
            return;
        }
        match c {
            '"' => self.literal = true,
            '“' => self.name = true,
            '*' if previous == Some('/') && self.depth > 0 => self.block_comment = true,
            '/' if previous == Some('/') && self.depth > 0 => self.line_comment = true,
            '(' | '[' => self.depth += 1,
            ')' | ']' => self.depth -= 1,
            _ => {}
        }
    }
}

/// The text cut into step records: one per line, a step whose brackets,
/// literals or comments run on continuing over the following lines.
/// Blank lines are empty comments; a final line break ends the text. A
/// step still open at the end (a stray quote or bracket) is its first line
/// alone, which does not read, and reading goes on from the next line, so
/// the stray character takes only its own line with it.
fn records(text: &str) -> Vec<String> {
    numbered_records(text)
        .into_iter()
        .map(|(_, record)| record)
        .collect()
}

/// [`parse_steps`]' records of a text form, each with the number of the
/// line it starts on (from 1), for reading steps one by one
/// ([`parse_record`]) and saying where a record that does not read is.
pub fn numbered_records(text: &str) -> Vec<(usize, String)> {
    // No text, no steps; a lone return is one empty comment.
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // Each stray character rescans to the end; past this many the rest
    // stays one record, so hostile text stays linear.
    const RESTARTS: usize = 32;
    let mut restarts = 0;
    let feed = |scanner: &mut Scanner, text: &str| {
        for c in text.chars() {
            scanner.feed(c);
        }
        scanner.feed('\n');
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let body = strip_prefixes(line).0;
        if body.starts_with('#') || body.is_empty() {
            out.push((i + 1, line.to_owned()));
            i += 1;
            continue;
        }
        let mut scanner = Scanner::default();
        feed(&mut scanner, body);
        let mut record = line.to_owned();
        let mut end = i + 1;
        while !scanner.at_top() && end < lines.len() {
            record.push('\n');
            record.push_str(lines[end]);
            feed(&mut scanner, lines[end]);
            end += 1;
        }
        if scanner.at_top() || restarts == RESTARTS {
            out.push((i + 1, record));
            i = end;
        } else {
            restarts += 1;
            out.push((i + 1, line.to_owned()));
            i += 1;
        }
    }
    out
}

/// A record without its indentation and `// ` prefix; whether it is
/// disabled.
fn strip_prefixes(record: &str) -> (&str, bool) {
    let body = record.trim_start_matches([' ', '\t']);
    match body.strip_prefix("//") {
        Some(rest) => (rest.strip_prefix(' ').unwrap_or(rest), true),
        None => (body, false),
    }
}

/// The step names, longest first.
fn names_by_length() -> &'static [(u16, &'static str)] {
    static NAMES: std::sync::OnceLock<Vec<(u16, &'static str)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<(u16, &'static str)> = crate::step_ids()
            .filter_map(|id| Some((id, crate::step_name(id)?)))
            .filter(|(_, name)| !name.starts_with('#'))
            .collect();
        names.sort_by_key(|(id, name)| (std::cmp::Reverse(name.len()), *id));
        names
    })
}

/// The step id a record's text starts with, and the rest. Ids FileMaker's
/// string table does not name read as `Unsupported step (n)`, as they show.
fn step_name(body: &str) -> Option<(u16, &str)> {
    if let Some(rest) = body.strip_prefix("Unsupported step (") {
        let (n, rest) = rest.split_once(')')?;
        return Some((n.parse().ok()?, rest));
    }
    names_by_length().iter().find_map(|(id, name)| {
        let head = body.get(..name.len())?;
        let rest = &body[name.len()..];
        (head.eq_ignore_ascii_case(name) && (rest.is_empty() || rest.starts_with([' ', '[', '\t'])))
            .then_some((*id, rest))
    })
}

/// The options inside a record's brackets, split at top-level `;`.
fn options(rest: &str) -> Result<Vec<String>, String> {
    let rest = rest.trim_start();
    let rest = rest.trim_end_matches([' ', '\t', '\r', '\n']);
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let inner = rest
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| format!("“{rest}” is not a bracketed list of options"))?;
    let mut out = Vec::new();
    let mut current = String::new();
    let mut scanner = Scanner::bracketed();
    for c in inner.chars() {
        if c == ';' && scanner.at(1) {
            out.push(option_text(&std::mem::take(&mut current)));
            continue;
        }
        scanner.feed(c);
        current.push(c);
    }
    if scanner.depth != 1 || scanner.literal || scanner.name || scanner.block_comment {
        return Err("The brackets, quotes or comments of the options do not close".into());
    }
    let last = option_text(&current);
    if !last.trim().is_empty() || !out.is_empty() {
        out.push(last);
    }
    // `[ ]` holds no option.
    if out.len() == 1 && out[0].trim().is_empty() {
        out.clear();
    }
    Ok(out)
}

/// An option as written between separators: without the one space the
/// text form puts on each side, and without the line break it adds after
/// a `//` comment. A calculation keeps any other whitespace it has.
fn option_text(piece: &str) -> String {
    let piece = piece.strip_prefix(' ').unwrap_or(piece);
    let piece = piece.strip_suffix(' ').unwrap_or(piece);
    if let Some(before) = piece.strip_suffix('\n') {
        let mut scanner = Scanner::bracketed();
        for c in before.chars() {
            scanner.feed(c);
        }
        if scanner.in_line_comment() {
            return before.to_owned();
        }
    }
    piece.to_owned()
}

/// The `Stored …` options of a record (anywhere among its options).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Extras {
    step: Option<ScriptStep>,
    flags: Option<u32>,
    value: Option<u16>,
    reserved: Option<[u8; 14]>,
    data: Option<Option<OwnedNode>>,
    slots: Vec<(u16, Option<OwnedNode>)>,
    keys: Vec<(u8, Option<OwnedNode>)>,
    /// Slots whose calculation is stored without its text (formatted
    /// stream), as FileMaker Server's samples store many.
    without_text: Vec<u16>,
}

/// A slot node with its calculation's formatted stream (`[05, 05]`)
/// removed.
fn strip_text(slot: &OwnedNode) -> OwnedNode {
    let mut slot = slot.clone();
    for child in &mut slot.children {
        if child.segment == [0x05] {
            child.children.retain(|c| c.segment != [0x05]);
        }
    }
    slot
}

impl Extras {
    fn is_empty(&self) -> bool {
        *self == Extras::default()
    }

    /// Whether these extras set `part`, so an issue about it is made up for.
    fn covers(&self, part: Part) -> bool {
        match part {
            Part::Reserved => self.reserved.is_some(),
            Part::Value => self.value.is_some(),
            Part::Flags => self.flags.is_some(),
            Part::Slot(n) => {
                self.data.is_some()
                    || self.slots.iter().any(|(s, _)| *s == n)
                    || self.without_text.contains(&n)
            }
            Part::Key(k) => self.data.is_some() || self.keys.iter().any(|(s, _)| *s == k),
        }
    }

    fn options(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(step) = &self.step {
            out.push(format!("Stored step: {}", base64(&raw_step(step))));
            return out;
        }
        let node = |n: &Option<OwnedNode>| match n {
            Some(n) => base64(&node_wire(n)),
            None => "-".into(),
        };
        if let Some(data) = &self.data {
            out.push(format!("Stored data: {}", node(data)));
        }
        for (n, slot) in &self.slots {
            out.push(format!("Stored slot {n}: {}", node(slot)));
        }
        for (k, key) in &self.keys {
            out.push(format!("Stored key {k:02x}: {}", node(key)));
        }
        for n in &self.without_text {
            out.push(format!("Stored without text: {n}"));
        }
        if let Some(value) = self.value {
            out.push(format!("Stored value: {value}"));
        }
        if let Some(reserved) = self.reserved {
            out.push(format!("Stored reference: {}", hex(&reserved)));
        }
        if let Some(flags) = self.flags {
            out.push(format!("Stored flags: 0x{flags:x}"));
        }
        out
    }

    /// Take the `Stored …` options out of `options`.
    fn take(options: &mut Vec<String>) -> Result<Extras, String> {
        let mut extras = Extras::default();
        let mut kept = Vec::new();
        for option in options.drain(..) {
            let Some(rest) = option.strip_prefix("Stored ") else {
                kept.push(option);
                continue;
            };
            let Some((label, value)) = rest.split_once(':') else {
                kept.push(option);
                continue;
            };
            let value = value.trim();
            let bad = || format!("“{option}” is not a stored value");
            let node = |v: &str| -> Result<Option<OwnedNode>, String> {
                if v == "-" {
                    return Ok(None);
                }
                unbase64(v)
                    .and_then(|b| wire_node(&b))
                    .map(Some)
                    .ok_or_else(bad)
            };
            match label {
                "step" => {
                    extras.step = Some(
                        unbase64(value)
                            .and_then(|b| unraw_step(&b))
                            .ok_or_else(bad)?,
                    )
                }
                "flags" => {
                    let hex = value.trim_start_matches("0x");
                    extras.flags = Some(u32::from_str_radix(hex, 16).map_err(|_| bad())?);
                }
                "value" => extras.value = Some(value.parse().map_err(|_| bad())?),
                "reference" => {
                    let bytes = unhex(value).ok_or_else(bad)?;
                    extras.reserved = Some(bytes.try_into().map_err(|_| bad())?);
                }
                "data" => extras.data = Some(node(value)?),
                "without text" => extras.without_text.push(value.parse().map_err(|_| bad())?),
                _ => {
                    if let Some(n) = label.strip_prefix("slot ") {
                        extras
                            .slots
                            .push((n.trim().parse().map_err(|_| bad())?, node(value)?));
                    } else if let Some(k) = label.strip_prefix("key ") {
                        let key = u8::from_str_radix(k.trim(), 16).map_err(|_| bad())?;
                        extras.keys.push((key, node(value)?));
                    } else {
                        kept.push(option);
                    }
                }
            }
        }
        *options = kept;
        Ok(extras)
    }

    /// Apply to a parsed step: data, slots, keys, value, reference, flags.
    fn apply(&self, step: &mut ScriptStep) {
        if let Some(data) = &self.data {
            step.data = data.as_ref().map(StepData::decode);
        }
        for (n, slot) in &self.slots {
            step.data_mut().set_slot_node(*n, slot.as_ref());
        }
        for (k, key) in &self.keys {
            step.data_mut().set_key_node(*k, key.clone());
        }
        for n in &self.without_text {
            if let Some(slot) = step.data.as_ref().and_then(|d| d.slot(*n)).cloned() {
                step.data_mut()
                    .set_slot_node(*n, Some(&strip_text(&slot.encode(*n))));
            }
        }
        if step.data.as_ref().is_some_and(StepData::is_empty) {
            step.data = None;
        }
        if let Some(value) = self.value {
            step.value = value;
        }
        if let Some(reserved) = self.reserved {
            step.reserved = reserved;
        }
        if let Some(flags) = self.flags {
            step.flags = flags;
        }
    }
}

/// A step as text and back compare: without its data key and the item keys
/// (`d7`-`da`: a pasted step is a new step), and without the indexes the
/// editor recomputes from the step's place (block steps' step value, Else
/// If's End If index, Go to Record and Go to Portal Row's End Loop index).
pub fn canonical(step: &ScriptStep) -> ScriptStep {
    let mut step = step.clone();
    step.data_key = crate::StoredKey::EMPTY;
    for key in StepData::ITEM_KEYS {
        if step.data_leaf(key).is_some() {
            if let Some(data) = &mut step.data {
                data.set_key_node(key, None);
            }
        }
    }
    let id = step.id();
    if crate::flow::caches_index(id) {
        step.value = 0;
    }
    if matches!(id, 16 | 99 | 125) && step.number(0).is_some() {
        if let Some(data) = &mut step.data {
            if let Some(slot) = data.slot(0).cloned() {
                let mut node = slot.encode(0);
                node.children.retain(|c| c.segment != [0x03]);
                if node.children.is_empty() && node.value.is_none() {
                    data.set_slot_node(0, None);
                } else {
                    node.push = !node.children.is_empty() || slot.encode(0).push;
                    data.set_slot_node(0, Some(&node));
                }
            }
        }
    }
    if step.data.as_ref().is_some_and(StepData::is_empty) {
        step.data = None;
    }
    step
}

/// What `parsed` needs to equal `target` (both canonical).
fn diff(target: &ScriptStep, parsed: &ScriptStep, issues: &[Issue]) -> Extras {
    let mut extras = Extras::default();
    let forced = |part: Part| issues.iter().any(|i| i.part == Some(part));
    let (td, pd) = (target.data.as_ref(), parsed.data.as_ref());
    // A step without data compares as empty data, so a missing slot is
    // carried alone.
    let empty = StepData::decode(&OwnedNode::list([0], Vec::new()));
    let roots_differ = match (td, pd) {
        (None, None) => false,
        (t, p) => {
            let (t, p) = (t.unwrap_or(&empty), p.unwrap_or(&empty));
            t.node_value() != p.node_value()
                || t.other() != p.other()
                || t.encode(&[0]).push != p.encode(&[0]).push
        }
    };
    if roots_differ {
        extras.data = Some(td.map(|d| d.encode(&[0])));
    } else if td.is_some() || pd.is_some() {
        let (t, p) = (td.unwrap_or(&empty), pd.unwrap_or(&empty));
        let mut slots: Vec<u16> = t
            .slots()
            .map(|(n, _)| n)
            .chain(p.slots().map(|(n, _)| n))
            .collect();
        slots.sort();
        slots.dedup();
        for n in slots {
            if t.slot(n) == p.slot(n) && !forced(Part::Slot(n)) {
                continue;
            }
            let stripped = match (t.slot(n), p.slot(n)) {
                (Some(t), Some(p)) => strip_text(&p.encode(n)) == t.encode(n),
                _ => false,
            };
            if stripped && !forced(Part::Slot(n)) {
                extras.without_text.push(n);
            } else {
                extras.slots.push((n, t.slot(n).map(|s| s.encode(n))));
            }
        }
        let mut keys: Vec<u8> = t.keys().chain(p.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            if t.key(k) != p.key(k) || forced(Part::Key(k)) {
                extras.keys.push((k, t.key(k).cloned()));
            }
        }
    }
    // Issues about parts the data has none of still need an answer.
    for issue in issues {
        match issue.part {
            Some(Part::Slot(n)) if !extras.covers(Part::Slot(n)) => {
                extras
                    .slots
                    .push((n, td.and_then(|d| d.slot(n)).map(|s| s.encode(n))));
            }
            Some(Part::Key(k)) if !extras.covers(Part::Key(k)) => {
                extras.keys.push((k, td.and_then(|d| d.key(k)).cloned()));
            }
            _ => {}
        }
    }
    if target.value != parsed.value || forced(Part::Value) {
        extras.value = Some(target.value);
    }
    if target.reserved != parsed.reserved || forced(Part::Reserved) {
        extras.reserved = Some(target.reserved);
    }
    if target.flags != parsed.flags || forced(Part::Flags) {
        extras.flags = Some(target.flags);
    }
    extras
}

/// How a step's text came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fidelity {
    /// The display line alone gives the step back.
    Display,
    /// `Stored …` options carry these parts.
    Extras(Vec<String>),
    /// The display line could not be read back: `Stored step` carries it.
    Raw(Vec<String>),
}

/// The step a display line reads back to, before `Stored …` options
/// (canonical), for diagnosing the text form.
pub fn display_reading(step: &ScriptStep, names: &dyn StepNames) -> Option<ScriptStep> {
    let line = text_line(step, names);
    read_record(&line, names).ok().map(|r| canonical(&r.step))
}

/// A step's line in the text form: its display line with a comment's
/// returns and Insert Text's quote characters escaped, and a line break
/// after each option that ends in a `//` comment (else the comment would
/// run on over the brackets).
fn text_line(step: &ScriptStep, names: &dyn StepNames) -> String {
    let line = super::line(step, names, super::Form::Text);
    let mut text = line.text.clone();
    if line.kind == super::LineKind::Comment {
        return text;
    }
    for option in line.options.iter().rev() {
        let mut scanner = Scanner::bracketed();
        for c in line.text[option.range.clone()].chars() {
            scanner.feed(c);
        }
        if scanner.in_line_comment() {
            text.insert(option.range.end, '\n');
        }
    }
    text
}

/// A step's text (no indentation) and how it came out.
pub fn step_text_fidelity(step: &ScriptStep, names: &dyn StepNames) -> (String, Fidelity) {
    let line = text_line(step, names);
    let target = canonical(step);
    let comment = step.id() == COMMENT_STEP;
    let raw = |reasons: Vec<String>| {
        let extras = Extras {
            step: Some(target.clone()),
            ..Extras::default()
        };
        // A comment carried whole names its step: `# text [ … ]` would be
        // read as the comment's text.
        let base = match (comment, step.is_enabled()) {
            (true, true) => "# (comment)".to_owned(),
            (true, false) => format!("{DISABLED}# (comment)"),
            (false, _) => line.clone(),
        };
        (
            with_options(&base, &extras.options()),
            Fidelity::Raw(reasons),
        )
    };
    let parsed = match read_record(&line, names) {
        Ok(parsed) => parsed,
        Err(reason) => return raw(vec![reason]),
    };
    if parsed.issues.iter().any(|i| i.part.is_none()) {
        return raw(parsed.issues.into_iter().map(|i| i.message).collect());
    }
    let extras = diff(&target, &canonical(&parsed.step), &parsed.issues);
    if comment && !extras.is_empty() {
        return raw(extras.options());
    }
    let text = with_options(&line, &extras.options());
    // The text must read back to the same step; anything else is carried
    // whole.
    match parse_record(&text, names, Origin::ThisFile) {
        Ok(step) if canonical(&step) == target => {
            if extras.is_empty() {
                (text, Fidelity::Display)
            } else {
                (text, Fidelity::Extras(extras.options()))
            }
        }
        Ok(_) => raw(vec!["The text reads back to a different step".into()]),
        Err(reason) => raw(vec![reason]),
    }
}

/// A step's text, without indentation.
pub fn step_text(step: &ScriptStep, names: &dyn StepNames) -> String {
    step_text_fidelity(step, names).0
}

/// Steps as text, one per line (a step whose calculation holds returns
/// runs on over several), indented with a tab per block level.
pub fn steps_text(steps: &[ScriptStep], names: &dyn StepNames) -> String {
    // Every line ends with a return, so a final empty comment survives.
    script_lines(steps, names)
        .iter()
        .zip(steps)
        .map(|(line, step)| {
            format!(
                "{}{}\n",
                TEXT_INDENT.repeat(line.depth),
                step_text(step, names)
            )
        })
        .collect()
}

/// `line` with `extra` options added inside its brackets (or in new ones).
fn with_options(line: &str, extra: &[String]) -> String {
    if extra.is_empty() {
        return line.to_owned();
    }
    let joined = extra.join(" ; ");
    match line.strip_suffix(" ]") {
        Some(head) if head.ends_with(" [") => format!("{head} {joined} ]"),
        Some(head) => format!("{head} ; {joined} ]"),
        None => format!("{line} [ {joined} ]"),
    }
}

/// A record read into a step, its issues and its `Stored …` options (not
/// applied yet).
struct Read {
    step: ScriptStep,
    issues: Vec<Issue>,
    extras: Extras,
}

fn read_record(record: &str, names: &dyn StepNames) -> Result<Read, String> {
    let (body, disabled) = strip_prefixes(record);
    let body = body.trim_end();
    let mut read = if body.is_empty() || (body.starts_with('#') && !body.starts_with("# (comment)"))
    {
        // `# text`, or a blank line: an empty comment.
        let mut step = ScriptStep::new(COMMENT_STEP);
        let text = body.strip_prefix('#').unwrap_or_default();
        let text = text.strip_prefix(' ').unwrap_or(text);
        step.set_text(0, Some(&unescape_comment(text)));
        Read {
            step,
            issues: Vec::new(),
            extras: Extras::default(),
        }
    } else {
        let (id, rest) = match body.strip_prefix("# (comment)") {
            Some(rest) => (COMMENT_STEP, rest),
            None => step_name(body)
                .ok_or_else(|| format!("“{body}” does not start with a script step's name"))?,
        };
        let mut opts = options(rest)?;
        let extras = Extras::take(&mut opts)?;
        let parser = Parser::new(names, id, opts).run();
        Read {
            step: parser.step,
            issues: parser.issues,
            extras,
        }
    };
    read.step.set_enabled(!disabled);
    if let Some(step) = &mut read.extras.step {
        // The carried step must be the step its line names.
        if step.id() != read.step.id() {
            return Err(format!(
                "Its stored step is {}, not {}",
                step.name(),
                read.step.name()
            ));
        }
        step.set_enabled(!disabled);
    }
    Ok(read)
}

/// Where a step's text comes from, which decides whether its `Stored …`
/// options may be taken as written: they hold the ids of the file the text
/// was written from, and text carries no proof of which file that was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The caller wrote the text from this file's own steps (the text
    /// form's round trip): every `Stored …` option is taken, losslessly.
    ThisFile,
    /// Text from anywhere else: the clipboard (the Script Workspace's own
    /// copy aside, which it pastes whole), a document, another file. A
    /// `Stored …` option is taken only where it leaves every file-local id
    /// (fields, table occurrences, layouts, scripts, custom functions, sort
    /// and find specs, file references, import and export orders) as the
    /// open file's names give them; otherwise the step fails whole. This
    /// includes Add Account's flagless privilege-set key. Unused inline
    /// reference bytes are dropped.
    Unknown,
}

/// Read one record (a step's text, its indentation and `// ` allowed).
pub fn parse_record(
    record: &str,
    names: &dyn StepNames,
    origin: Origin,
) -> Result<ScriptStep, String> {
    let read = read_record(record, names)?;
    let resolved = read.step.clone();
    let mut step = match read.extras.step {
        Some(step) => step,
        None => {
            // From elsewhere, a reference the text names but this file
            // does not have is not made up for by stored bytes.
            let covered = |part: Part| {
                read.extras.covers(part) && !(origin == Origin::Unknown && part == Part::Reserved)
            };
            let unread = read.issues.iter().find(|i| match i.part {
                Some(part) => !covered(part),
                None => true,
            });
            if let Some(issue) = unread {
                return Err(issue.message.clone());
            }
            let mut step = read.step;
            read.extras.apply(&mut step);
            step
        }
    };
    if origin == Origin::Unknown {
        from_elsewhere(&mut step, &resolved)?;
    }
    // A pasted step is a new step: no UUIDs, owner or tags of another.
    if let Some(data) = &mut step.data {
        for key in StepData::ITEM_KEYS {
            data.set_key_node(key, None);
        }
        if data.is_empty() {
            step.data = None;
        }
    }
    Ok(step)
}

/// Take a step read from text of unknown origin only where its `Stored …`
/// options hold the file-local ids the open file's names give. Where the
/// text named a part that the options hold under other ids, the bytes can
/// only be another file's (this file's text names its own objects, and an
/// ambiguous name already failed), so the part is taken as the names give
/// it. Ids the text names nothing for fail the step.
fn from_elsewhere(step: &mut ScriptStep, resolved: &ScriptStep) -> Result<(), String> {
    // Add Account's privilege set is a key in `mReserved` that no flag
    // marks (local-files.md 2.4), a file-local id its text does not name:
    // from another file it cannot be checked, and dropping it would add the
    // account without its set.
    if step.id() == 134 && step.reserved != resolved.reserved {
        return Err(
            "Its stored privilege set is an id its text does not name in this file \
             (the text may come from another file)"
                .into(),
        );
    }
    if step.flags & REFERENCES == 0 {
        step.reserved = resolved.reserved;
    }
    let named_slot = |n: u16, key: u8| {
        resolved
            .data
            .as_ref()
            .and_then(|d| d.slot(n))
            .and_then(|slot| slot.child(key))
            .cloned()
    };
    let ours = file_ids(resolved);
    let mut conflicting = Vec::new();
    for (at, ids) in file_ids(step) {
        if let Place::Slot(n, 0x05) = at {
            // A calculation is compared as code, not as the set of ids it
            // names: the same ids can be bound to other names.
            match bound(step.calculation(n), resolved.calculation(n)) {
                Some(Bound::Renamed) => step.set_slot_child(n, 0x05, named_slot(n, 0x05)),
                Some(Bound::Conflicting) => conflicting.push(at.describe(step)),
                Some(Bound::Named) | None => {}
            }
            continue;
        }
        if ours.get(&at) == Some(&ids) || !ours.contains_key(&at) {
            continue;
        }
        match at {
            Place::Reference => step.reserved = resolved.reserved,
            Place::Slot(n, key @ (0x10 | 0x11)) => step.set_slot_child(n, key, named_slot(n, key)),
            Place::Slot(..) | Place::Key(_) | Place::Other => {}
        }
    }
    if !conflicting.is_empty() {
        return Err(format!(
            "Its stored code for {} is not what its text shows, so either could be meant",
            conflicting.join(", ")
        ));
    }
    let ours = file_ids(resolved);
    let foreign: Vec<String> = file_ids(step)
        .into_iter()
        .filter(|(at, ids)| match at {
            Place::Slot(n, 0x05) => {
                bound(step.calculation(*n), resolved.calculation(*n)) != Some(Bound::Named)
            }
            _ => ours.get(at) != Some(ids),
        })
        .map(|(at, _)| at.describe(step))
        .collect();
    if foreign.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Its Stored options refer by id to {} that its text does not name in this file \
         (the text may come from another file)",
        foreign.join(", ")
    ))
}

/// The flags that make `mReserved` a reference.
const REFERENCES: u32 = flag::FIELD_SPEC | flag::MEMBER_KEY | flag::SCRIPT_KEY;

/// The step-level keys that hold no file-local ids: path strings, compiled
/// blobs (AppleScript, plug-ins), import profiles, page formats and print
/// settings, Insert File's filters; the item UUIDs are a step's own.
const ID_FREE_KEYS: [u8; 10] = [
    StepData::PATH_STRING,
    StepData::BLOB,
    StepData::PROFILE,
    StepData::PAGE_FORMAT,
    StepData::PRINT_SETTINGS,
    StepData::FILE_FILTER_SPEC,
    StepData::OWNER_ID,
    StepData::UNIQUE_ID,
    StepData::SOURCE_UUID,
    StepData::TAGS,
];

/// Where a step holds file-local ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    /// `mReserved`, when a flag makes it a reference.
    Reference,
    /// A slot's child: `05` calculation, `10` field, `11` occurrence, or
    /// another.
    Slot(u16, u8),
    /// A step-level key.
    Key(u8),
    /// Data outside slots and keys.
    Other,
}

impl Place {
    fn describe(self, step: &ScriptStep) -> String {
        match self {
            Place::Reference if step.has_flag(flag::FIELD_SPEC) => "its field".into(),
            Place::Reference if step.has_flag(flag::SCRIPT_KEY) => "its script".into(),
            Place::Reference => "its layout, table or menu set".into(),
            Place::Slot(n, 0x05) => format!("the calculation of slot {n}"),
            Place::Slot(n, 0x10) => format!("the field of slot {n}"),
            Place::Slot(n, 0x11) => format!("the table occurrence of slot {n}"),
            Place::Slot(n, key) => format!("data {key:02x} of slot {n}"),
            Place::Key(StepData::REQUESTS) => "its find requests".into(),
            Place::Key(StepData::SORT_ORDER) => "its sort order".into(),
            Place::Key(StepData::FILE_REFERENCES) => "its file reference".into(),
            Place::Key(StepData::IMPORT_ORDER) => "its import order".into(),
            Place::Key(StepData::EXPORT_ORDER) => "its export order".into(),
            Place::Key(key) => format!("its data under key {key:02x}"),
            Place::Other => "its other data".into(),
        }
    }
}

/// The file-local ids a step refers to, by where it holds them: what a
/// step from another file must not carry over as bytes. Parts whose ids
/// are not decoded are kept whole.
fn file_ids(step: &ScriptStep) -> BTreeMap<Place, String> {
    let mut out = BTreeMap::new();
    // A cleared spec or key (id 0, or none) names nothing.
    let named = |id: Option<u64>| id.filter(|id| *id != 0);
    let spec_ids = |spec: FieldSpec| {
        let ids = (named(spec.table().id()), named(spec.field().id()));
        (!spec.is_variable() && ids != (None, None)).then_some(ids)
    };
    if let Some(ids) = step.field_spec().and_then(spec_ids) {
        out.insert(Place::Reference, format!("{ids:?}"));
    } else if let Some(id) = named(
        step.member_key()
            .or_else(|| step.script_key())
            .and_then(|k| k.id()),
    ) {
        out.insert(Place::Reference, format!("{id:?}"));
    }
    let Some(data) = &step.data else {
        return out;
    };
    for (n, slot) in data.slots() {
        for key in slot.child_keys() {
            let node = slot.child(key).expect("listed");
            let ids = match key {
                0x01 | 0x03 | 0x82 => continue,
                0x05 => match step.calculation(n).map(|c| calculation_ids(&c)) {
                    Some(Some(ids)) if ids.is_empty() => continue,
                    Some(Some(ids)) => format!("{ids:?}"),
                    _ => format!("{node:?}"),
                },
                0x10 => match step.slot_field_spec(n).map(spec_ids) {
                    Some(None) => continue,
                    Some(Some(ids)) => format!("{ids:?}"),
                    None => format!("{node:?}"),
                },
                0x11 => match crate::value::node_bytes(node)
                    .map(|b| om_formula::Key(b.into_owned()).id())
                {
                    Some(id) if named(id).is_none() => continue,
                    _ => format!("{node:?}"),
                },
                _ => format!("{node:?}"),
            };
            out.insert(Place::Slot(n, key), ids);
        }
        if !slot.other().is_empty() {
            out.insert(Place::Slot(n, 0xff), format!("{:?}", slot.other()));
        }
    }
    for key in data.keys() {
        if ID_FREE_KEYS.contains(&key) {
            continue;
        }
        let node = data.key(key).expect("listed");
        if key == StepData::SORT_ORDER
            && crate::specs::SortSpec::from_node(node).is_some_and(|s| s.fields.is_empty())
        {
            continue;
        }
        out.insert(Place::Key(key), format!("{node:?}"));
    }
    if !data.other().is_empty() {
        out.insert(Place::Other, format!("{:?}", data.other()));
    }
    out
}

/// A calculation's id: its context occurrence (0), a field (1: occurrence
/// and field), a custom function (2) or a plug-in function (3: plug-in and
/// function number). A plug-in's id is not the file's, but a call the text
/// cannot name is code it does not show.
type CalcId = (u8, Option<u64>, Option<u64>);

/// The ids a calculation names, stream by stream, each in the order the
/// stream holds them: its context, its formatted stream (`None` when
/// stripped) and its Polish stream (`None` when absent).
#[derive(Debug, PartialEq, Eq)]
struct CalcIds {
    context: Vec<CalcId>,
    formatted: Option<Vec<CalcId>>,
    polish: Option<Vec<CalcId>>,
}

impl CalcIds {
    fn is_empty(&self) -> bool {
        self.context.is_empty()
            && self
                .formatted
                .iter()
                .chain(&self.polish)
                .all(|ids| ids.is_empty())
    }
}

/// How a stored calculation stands to the one its text compiles to here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    /// The same code naming the same ids.
    Named,
    /// The same code with other ids in their places, one for one: another
    /// file's ids, so it is taken as the names give it.
    Renamed,
    /// Other code, or ids that do not stand one for one for the named ones
    /// (the code binds fields its text does not show there): its text and
    /// code disagree, and either could be meant.
    Conflicting,
}

/// How `stored` stands to `named`, the calculation its text compiles to;
/// `None` when either is missing. The two are compared as code with the
/// ids cleared, then id by id in the order each stream holds them: a set
/// of ids cannot tell First Name and Last Name from the same two fields
/// the other way round.
fn bound(stored: Option<crate::Calculation>, named: Option<crate::Calculation>) -> Option<Bound> {
    let (stored, named) = (stored?, named?);
    let (Some(ids), Some(named_ids)) = (calculation_ids(&stored), calculation_ids(&named)) else {
        return Some(Bound::Conflicting);
    };
    match (shape(&stored), shape(&named)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Some(Bound::Conflicting),
    }
    // A stream the stored calculation lacks has no ids to answer for.
    let mut pairs = Vec::new();
    for (stream, named) in [
        (Some(&ids.context), Some(&named_ids.context)),
        (ids.formatted.as_ref(), named_ids.formatted.as_ref()),
        (ids.polish.as_ref(), named_ids.polish.as_ref()),
    ] {
        match (stream, named) {
            (None, _) => {}
            (Some(a), Some(b)) if a.len() == b.len() => pairs.extend(a.iter().zip(b)),
            _ => return Some(Bound::Conflicting),
        }
    }
    let mut to_named = HashMap::new();
    let mut to_stored = HashMap::new();
    for (a, b) in pairs {
        if a.0 != b.0
            || *to_named.entry(a).or_insert(b) != b
            || *to_stored.entry(b).or_insert(a) != a
        {
            return Some(Bound::Conflicting);
        }
    }
    if to_named.iter().all(|(a, b)| a == b) {
        Some(Bound::Named)
    } else {
        Some(Bound::Renamed)
    }
}

/// A calculation's Polish code with the field, occurrence and custom
/// function ids it names cleared (a plug-in function stays: no file names
/// it otherwise), and with how its names were quoted (`${ }`) cleared, for
/// the text prints names without it; `None` when it has none or it does
/// not decode.
fn shape(calc: &crate::Calculation) -> Option<Vec<u8>> {
    use om_formula::{Expr, Field};
    fn clear(expr: &mut Expr) {
        let none = || om_formula::Key::from_id(0);
        match expr {
            Expr::Field(f) | Expr::FieldRef(f) => {
                f.flags &= !(Field::FIELD_BRACKETED | Field::TABLE_BRACKETED);
                f.occurrence = none();
                f.field = none();
            }
            Expr::CustomCall { function, args, .. } => {
                *function = none();
                args.iter_mut().for_each(clear);
            }
            Expr::List(items)
            | Expr::Call { args: items, .. }
            | Expr::Evaluate { args: items, .. }
            | Expr::PluginCall { args: items, .. } => items.iter_mut().for_each(clear),
            Expr::Case(pairs) => pairs.iter_mut().for_each(|(a, b)| {
                clear(a);
                clear(b);
            }),
            Expr::Choose { test, results } => {
                clear(test);
                results.iter_mut().for_each(clear);
            }
            Expr::Let { bindings, body } => {
                bindings.iter_mut().for_each(clear);
                clear(body);
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => {
                initial.iter_mut().chain(logic).for_each(clear);
                clear(condition);
                clear(result);
            }
            Expr::GetField { arg, .. } => clear(arg),
            _ => {}
        }
    }
    // The decoder bounds the nesting, so the walk does too.
    let mut expr = om_formula::polish::decode(calc.polish.as_deref()?).ok()?;
    clear(&mut expr);
    Some(om_formula::polish::encode(&expr))
}

/// A plug-in function as a [`CalcId`].
fn plugin_id(id: om_formula::PluginFunctionId) -> CalcId {
    (
        3,
        Some(u32::from_be_bytes(id.plugin).into()),
        Some(id.function.into()),
    )
}

/// The fields, table occurrences, custom functions and plug-in functions a
/// calculation names by id, from its context, formatted and Polish streams
/// (the two streams name the same ones, and a calculation stored without
/// its text has only the second); `None` when a stream does not decode.
fn calculation_ids(calc: &crate::Calculation) -> Option<CalcIds> {
    use om_formula::formatted::Token;
    use om_formula::Expr;
    let mut context = Vec::new();
    if let Some(key) = calc
        .context
        .as_deref()
        .filter(|c| *c != crate::Calculation::NO_CONTEXT)
    {
        context.push((
            0,
            crate::StoredKey::from_bytes(key).and_then(|k| k.id()),
            None,
        ));
    }
    let formatted = match &calc.formatted {
        Some(formatted) => Some(
            om_formula::formatted::decode(formatted)
                .ok()?
                .into_iter()
                .filter_map(|token| match token {
                    Token::Field(f) => Some((1, f.occurrence.id(), f.field.id())),
                    Token::CustomFunction(k) => Some((2, k.id(), None)),
                    Token::PluginFunction(id) => Some(plugin_id(id)),
                    _ => None,
                })
                .collect(),
        ),
        None => None,
    };
    fn walk(expr: &Expr, ids: &mut Vec<CalcId>) {
        match expr {
            Expr::Field(f) | Expr::FieldRef(f) => ids.push((1, f.occurrence.id(), f.field.id())),
            Expr::CustomCall { function, args, .. } => {
                ids.push((2, function.id(), None));
                args.iter().for_each(|a| walk(a, ids));
            }
            Expr::PluginCall { function, args } => {
                ids.push(plugin_id(*function));
                args.iter().for_each(|a| walk(a, ids));
            }
            Expr::List(items)
            | Expr::Call { args: items, .. }
            | Expr::Evaluate { args: items, .. } => items.iter().for_each(|a| walk(a, ids)),
            Expr::Case(pairs) => pairs.iter().for_each(|(a, b)| {
                walk(a, ids);
                walk(b, ids);
            }),
            Expr::Choose { test, results } => {
                walk(test, ids);
                results.iter().for_each(|r| walk(r, ids));
            }
            Expr::Let { bindings, body } => {
                bindings.iter().for_each(|b| walk(b, ids));
                walk(body, ids);
            }
            Expr::While {
                initial,
                condition,
                logic,
                result,
            } => {
                initial.iter().chain(logic).for_each(|b| walk(b, ids));
                walk(condition, ids);
                walk(result, ids);
            }
            Expr::GetField { arg, .. } => walk(arg, ids),
            _ => {}
        }
    }
    let polish = match &calc.polish {
        Some(polish) => {
            // The decoder bounds the nesting, so the walk does too.
            let mut ids = Vec::new();
            walk(&om_formula::polish::decode(polish).ok()?, &mut ids);
            Some(ids)
        }
        None => None,
    };
    Some(CalcIds {
        context,
        formatted,
        polish,
    })
}

/// The most text a paste reads.
pub const MAX_PASTE_BYTES: usize = 8 << 20;
/// The most steps a paste inserts.
pub const MAX_PASTE_STEPS: usize = 20_000;

/// A pasted text's steps: FileMaker's XML (`fmxmlsnippet`, Save as XML
/// `<Step>` elements), or the text form, one record per step. A record
/// that cannot be read is kept as [`Pasted::Failed`]. A text past
/// [`MAX_PASTE_BYTES`] or [`MAX_PASTE_STEPS`], or XML nested past what
/// FileMaker writes, is refused whole with the reason: it is neither read
/// slowly nor kept as one huge comment.
pub fn parse_steps(
    text: &str,
    names: &dyn StepNames,
    origin: Origin,
) -> Result<Vec<Pasted>, String> {
    too_much_text(text)?;
    if text.trim_start().starts_with('<') {
        return parse_xml_steps(text, names);
    }
    let records = records(text);
    too_many_steps(records.len())?;
    Ok(records
        .into_iter()
        .map(|record| match parse_record(&record, names, origin) {
            Ok(step) => Pasted::Step(step),
            Err(reason) => Pasted::Failed {
                text: record,
                reason,
            },
        })
        .collect())
}

/// Refuse a paste past the limits [`parse_steps`] holds a text to: more
/// than [`MAX_PASTE_BYTES`] of text or [`MAX_PASTE_STEPS`] steps. For a
/// paste of steps its text was written from (the Script Workspace's own
/// copy), which inserts them without reading the text.
pub fn paste_limits(text: &str, steps: usize) -> Result<(), String> {
    too_much_text(text)?;
    too_many_steps(steps)
}

/// Refuse a paste of more than [`MAX_PASTE_BYTES`] of text.
fn too_much_text(text: &str) -> Result<(), String> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(format!(
            "The clipboard holds {} KB of text; a paste reads at most {} KB.",
            text.len().div_ceil(1024),
            MAX_PASTE_BYTES / 1024
        ));
    }
    Ok(())
}

/// Refuse a paste of more than [`MAX_PASTE_STEPS`] steps.
pub(super) fn too_many_steps(steps: usize) -> Result<(), String> {
    if steps > MAX_PASTE_STEPS {
        return Err(format!(
            "The clipboard holds {steps} steps; a paste inserts at most {MAX_PASTE_STEPS}."
        ));
    }
    Ok(())
}

/// A line that could not be read, kept as a disabled comment holding it.
pub fn failed_comment(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(COMMENT_STEP);
    step.set_text(0, Some(&text.replace("\r\n", "\r").replace('\n', "\r")));
    step.set_enabled(false);
    step
}

/// A step's descriptor (data key cleared) and data subtree, as `Stored
/// step` carries it.
fn raw_step(step: &ScriptStep) -> Vec<u8> {
    let mut step = step.clone();
    step.data_key = crate::StoredKey::EMPTY;
    let mut out = step.descriptor().to_vec();
    if let Some(data) = &step.data {
        out.extend(node_wire(&data.encode(&[0])));
    }
    out
}

fn unraw_step(bytes: &[u8]) -> Option<ScriptStep> {
    let descriptor: &[u8; 28] = bytes.get(..28)?.try_into().ok()?;
    let mut step = ScriptStep::from_descriptor(descriptor);
    if bytes.len() > 28 {
        step.data = Some(StepData::decode(&wire_node(&bytes[28..])?));
    }
    Some(step)
}

/// A node in the wire form, as the only child of a root.
fn node_wire(node: &OwnedNode) -> Vec<u8> {
    OwnedNode::list(Vec::new(), vec![node.clone()])
        .to_wire()
        .expect("a step's node encodes")
}

fn wire_node(bytes: &[u8]) -> Option<OwnedNode> {
    let tree = KeyTree::from_slots(SlotReader::new(bytes)).ok()?;
    Some(tree.root.children.first()?.to_owned_node())
}

/// URL-safe base64: no `/`, so a stored value never reads as a comment.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = BASE64.iter().position(|b| *b == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::NoStepNames;

    #[test]
    fn a_stored_step_must_be_the_step_its_line_names() {
        let mut set = ScriptStep::new(76);
        set.set_text(1, Some("x"));
        let carried = base64(&raw_step(&set));
        let line = format!("Beep [ Stored step: {carried} ]");
        for origin in [Origin::ThisFile, Origin::Unknown] {
            assert_eq!(
                parse_record(&line, &NoStepNames, origin).unwrap_err(),
                "Its stored step is Set Field, not Beep"
            );
        }
        let line = format!("Set Field [ Stored step: {carried} ]");
        assert_eq!(
            parse_record(&line, &NoStepNames, Origin::ThisFile).unwrap(),
            set
        );
    }

    #[test]
    fn pasted_steps_carry_no_item_keys() {
        // A UUID (d8) and an owner (d7), stored as text by hand or by
        // another writer: a pasted step is new and has neither.
        let uuid = OwnedNode::leaf([StepData::UNIQUE_ID], [7; 16]);
        let owner = OwnedNode::leaf([StepData::OWNER_ID], [1, 2]);
        let line = format!(
            "Beep [ Stored key d8: {} ; Stored key d7: {} ]",
            base64(&node_wire(&uuid)),
            base64(&node_wire(&owner))
        );
        let mut beep = ScriptStep::new(93);
        beep.set_data_leaf(StepData::UNIQUE_ID, Some(uuid.clone()));
        let carried = format!("Beep [ Stored step: {} ]", base64(&raw_step(&beep)));
        for origin in [Origin::ThisFile, Origin::Unknown] {
            for text in [&line, &carried] {
                let step = parse_record(text, &NoStepNames, origin).unwrap();
                assert_eq!(step.unique_id(), None, "{text}");
                assert_eq!(step.data, None, "{text}");
            }
        }
    }

    #[test]
    fn text_from_elsewhere_takes_no_ids_its_names_do_not_give() {
        // References this file (no names) cannot name, so their text
        // carries them as `Stored …` options: a field, a script of another
        // file, a data source, a menu set. As this file's own text each
        // comes back; from elsewhere each fails rather than take ids that
        // may be another file's.
        let field = FieldSpec::new(
            crate::StoredKey([4, 3, 0xd0, 0, 2, 0]),
            crate::StoredKey([2, 1, 2, 0, 0, 0]),
            1,
        );
        let source = OwnedNode::leaf([StepData::FILE_REFERENCES], [1, 2]);
        let mut set = ScriptStep::new(76);
        set.set_field_spec(0, Some(field));
        let mut from_file = ScriptStep::new(1);
        from_file.set_script_key(crate::StoredKey::from_serial_id(2));
        from_file.set_data_leaf(StepData::FILE_REFERENCES, Some(source.clone()));
        let mut close = ScriptStep::new(34);
        close.set_data_leaf(StepData::FILE_REFERENCES, Some(source));
        let mut menus = ScriptStep::new(142);
        menus.set_member_key(crate::StoredKey::from_id(5));
        for step in [&set, &from_file, &close, &menus] {
            let text = step_text(step, &NoStepNames);
            assert!(text.contains("Stored"), "{text}");
            let back = parse_record(&text, &NoStepNames, Origin::ThisFile).unwrap();
            assert_eq!(canonical(&back), canonical(step), "{text}");
            assert!(
                parse_record(&text, &NoStepNames, Origin::Unknown).is_err(),
                "{text}"
            );
        }
        // Carried whole, the same.
        let carried = format!("Set Field [ Stored step: {} ]", base64(&raw_step(&set)));
        assert_eq!(
            canonical(&parse_record(&carried, &NoStepNames, Origin::ThisFile).unwrap()),
            canonical(&set)
        );
        let reason = parse_record(&carried, &NoStepNames, Origin::Unknown).unwrap_err();
        assert!(reason.contains("its field"), "{reason}");
    }

    #[test]
    fn a_path_longer_than_its_length_prefix_fails_its_step() {
        let longest = "a".repeat(super::super::parse::MAX_PATH_BYTES);
        let line =
            |path: &str| format!("Import Records [ With dialog: On ; Source: “file:{path}” ]");
        let step = parse_record(&line(&longest), &NoStepNames, Origin::Unknown).unwrap();
        let stream = node_bytes_of(&step);
        // u32 count, then the kind `file` (4 masked bytes), the volume (0)
        // and the path with its two-byte prefix: 0x80 | the high six bits,
        // the low byte.
        assert_eq!(&stream[..4], [0, 0, 0, 1]);
        assert_eq!(stream[4], 4);
        assert_eq!(&stream[5..9], om_scsu::encode_scsu("file"));
        assert_eq!(stream[9], 0);
        assert_eq!(&stream[10..12], [0xbf, 0xff]);
        assert_eq!(stream.len(), 12 + longest.len());
        let longer = format!("{longest}a");
        let reason = parse_record(&line(&longer), &NoStepNames, Origin::Unknown).unwrap_err();
        assert_eq!(
            reason,
            "A file path of 16384 bytes is longer than the 16383 a step stores"
        );
    }

    /// The path list stream of a step's slot 0.
    fn node_bytes_of(step: &ScriptStep) -> Vec<u8> {
        let slot = step.data.as_ref().unwrap().slot(0).unwrap();
        crate::value::node_bytes(slot.child(StepData::PATH_STRING).unwrap())
            .unwrap()
            .into_owned()
    }

    #[test]
    fn base64_and_comment_escapes_round_trip() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd\xff\x00"] {
            assert_eq!(unbase64(&base64(bytes)).unwrap(), bytes);
        }
        for text in ["plain", "two\rlines", "a ¶ b", "a\\¶", "end\\", "\\\\x"] {
            assert_eq!(
                unescape_comment(&escape_comment(text)),
                text.replace('\n', "\r")
            );
        }
    }

    #[test]
    fn a_stray_quote_or_bracket_takes_only_its_own_line() {
        assert_eq!(
            records("Set Variable [ $x ; Value: \"oops ]\nBeep\nIf [ (1 ]\nBeep\n"),
            [
                "Set Variable [ $x ; Value: \"oops ]",
                "Beep",
                "If [ (1 ]",
                "Beep"
            ]
        );
        // A step that closes on a later line still runs on.
        assert_eq!(
            records("If [ Case (\n1 ; 2 ) ]\nBeep"),
            ["If [ Case (\n1 ; 2 ) ]", "Beep"]
        );
        // Past the restarts, the rest is one record: hostile text stays
        // linear.
        let strays = "If [ (\n".repeat(40);
        assert_eq!(records(&strays).len(), 33);
    }

    #[test]
    fn records_run_on_while_brackets_or_literals_are_open() {
        let text = "If [ Case (\n\tx ;\n\t\"a]\" ) ]\n\t# note [\nExit Script [ Text Result: /* ] */ 1 ]\n\n";
        assert_eq!(
            records(text),
            [
                "If [ Case (\n\tx ;\n\t\"a]\" ) ]",
                "\t# note [",
                "Exit Script [ Text Result: /* ] */ 1 ]",
                "",
            ]
        );
        assert_eq!(
            options("[ a ; Left ( b ; 1 ) ; \"c;d\" ; “e;f” ]").unwrap(),
            ["a", "Left ( b ; 1 )", "\"c;d\"", "“e;f”"]
        );
        assert!(options("[ ]").unwrap().is_empty());
        assert_eq!(step_name("Set Field By Name [ x ]").unwrap().0, 147);
        assert_eq!(step_name("set field [ x ]").unwrap().0, 76);
        assert!(step_name("Setting [ ]").is_none());
    }
}
