//! The step text's inverse: a step's options, as the formatter writes them
//! (`steps.rs`), read back into its typed parameters. Each rule below
//! mirrors the formatter's rule for the same step ids. What a rule cannot
//! express (stale bytes, opaque specs, unmapped flags) the text carries in
//! `Stored …` options (`text.rs`).

use super::{StepNames, UNKNOWN};
use crate::key::{FieldSpec, StoredKey};
use crate::runtime::WindowOptions;
use crate::step::{flag, Calculation, ScriptStep, VARIABLE_NAME_SLOT};
use crate::value::bytes_node;

/// The part of a step an issue leaves unset; a `Stored …` option for that
/// part makes up for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Part {
    Reserved,
    Value,
    Flags,
    Slot(u16),
    Key(u8),
}

/// Why a step's text could not be read in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub message: String,
    /// The part left unset, `None` for text that was not understood.
    pub part: Option<Part>,
}

/// Reads one step's options into a step of its id.
pub(super) struct Parser<'a> {
    names: &'a dyn StepNames,
    opts: Vec<String>,
    pos: usize,
    pub step: ScriptStep,
    pub issues: Vec<Issue>,
}

/// Go to Layout's animation names, in the formatter's order.
const ANIMATIONS: [&str; 11] = [
    "None",
    "Slide in from Left",
    "Slide in from Right",
    "Slide in from Bottom",
    "Slide out to Left",
    "Slide out to Right",
    "Slide out to Bottom",
    "Flip from Left",
    "Flip from Right",
    "Zoom In",
    "Zoom Out",
];

/// A one-segment key with zero filler, the filler most stored references
/// carry (corpus: 1,488 of 4,000 field specs and 1,090 of 1,300 member and
/// script keys).
fn zero_key(segment: &[u8]) -> Option<StoredKey> {
    if segment.is_empty() || segment.len() > 4 {
        return None;
    }
    let mut bytes = [0u8; 6];
    bytes[0] = segment.len() as u8 + 1;
    bytes[1] = segment.len() as u8;
    bytes[2..2 + segment.len()].copy_from_slice(segment);
    Some(StoredKey(bytes))
}

pub(super) fn id_key(id: u64) -> Option<StoredKey> {
    zero_key(&om_hbam::slot::uint_segment(id))
}

pub(super) fn serial_key(id: u64) -> Option<StoredKey> {
    zero_key(&om_hbam::serial::encode(id))
}

/// A typographic-quoted name, `“Name”`, unquoted.
fn unquote(text: &str) -> Option<&str> {
    text.trim().strip_prefix('“')?.strip_suffix('”')
}

/// A name followed by a parenthesized remark: `“Name” (Occurrence)`.
fn quoted_head(text: &str) -> Option<&str> {
    quoted_remark(text).map(|(name, _)| name)
}

/// `“Name”` or `“Name” (Remark)`: the name and the remark.
fn quoted_remark(text: &str) -> Option<(&str, Option<&str>)> {
    let rest = text.trim().strip_prefix('“')?;
    let end = rest.find('”')?;
    let remark = rest[end + '”'.len_utf8()..].trim();
    let remark = match remark {
        "" => None,
        _ => Some(remark.strip_prefix('(')?.strip_suffix(')')?),
    };
    Some((&rest[..end], remark))
}

/// `label: value` when `option` has that label (case-insensitive), the
/// value trimmed (empty for `label:`).
fn label_value<'o>(option: &'o str, label: &str) -> Option<&'o str> {
    let option = option.trim_start();
    let head = option.get(..label.len())?;
    if !head.eq_ignore_ascii_case(label) {
        return None;
    }
    let rest = &option[label.len()..];
    let rest = rest.trim_start_matches(' ').strip_prefix(':')?;
    // One space follows the colon; a calculation keeps the rest as is.
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    Some(if rest.trim().is_empty() { "" } else { rest })
}

impl<'a> Parser<'a> {
    pub fn new(names: &'a dyn StepNames, id: u16, opts: Vec<String>) -> Self {
        Self {
            names,
            opts,
            pos: 0,
            step: ScriptStep::new(id),
            issues: Vec::new(),
        }
    }

    /// Read the options; options left over are issues.
    pub fn run(mut self) -> Self {
        rule(&mut self);
        while self.pos < self.opts.len() {
            let option = self.opts[self.pos].clone();
            self.issue(format!("“{option}” is not an option of this step"), None);
            self.pos += 1;
        }
        self
    }

    fn issue(&mut self, message: String, part: Option<Part>) {
        self.issues.push(Issue { message, part });
    }

    fn peek(&self) -> Option<&str> {
        self.opts.get(self.pos).map(String::as_str)
    }

    fn next_is(&self, word: &str) -> bool {
        self.peek()
            .is_some_and(|p| p.trim().eq_ignore_ascii_case(word))
    }

    fn take(&mut self) -> Option<String> {
        let option = self.opts.get(self.pos).cloned();
        if option.is_some() {
            self.pos += 1;
        }
        option
    }

    /// `label: value`, consumed.
    fn labeled_value(&mut self, label: &str) -> Option<String> {
        let value = label_value(self.peek()?, label)?.to_owned();
        self.pos += 1;
        Some(value)
    }

    /// Whether the next option is one of `stop` (words, or `label:`).
    fn stops(&self, stop: &[&str]) -> bool {
        self.peek().is_some_and(|p| {
            stop.iter().any(|s| {
                p.trim().eq_ignore_ascii_case(s)
                    || (s.ends_with(':') && label_value(p, &s[..s.len() - 1]).is_some())
            })
        })
    }

    fn word(&mut self, word: &str, bits: u32) -> bool {
        if self.next_is(word) {
            self.step.set_flag(bits, true);
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `label: On|Off` for `bits` (set when On).
    fn on_off(&mut self, label: &str, bits: u32) {
        if let Some(value) = self.labeled_value(label) {
            match value.to_ascii_lowercase().as_str() {
                "on" => self.step.set_flag(bits, true),
                "off" => self.step.set_flag(bits, false),
                _ => self.issue(format!("{label} is On or Off"), Some(Part::Flags)),
            }
        }
    }

    /// `With dialog: On|Off`, the inverse of `kNoInteract`.
    fn dialog(&mut self) {
        if let Some(value) = self.labeled_value("With dialog") {
            match value.to_ascii_lowercase().as_str() {
                "on" => self.step.set_flag(flag::NO_INTERACT, false),
                "off" => self.step.set_flag(flag::NO_INTERACT, true),
                _ => self.issue("With dialog is On or Off".into(), Some(Part::Flags)),
            }
        }
    }

    fn select(&mut self) {
        self.word("Select", flag::SELECT_ALL);
    }

    /// `On` or `Off` of the on/off steps.
    fn state(&mut self) {
        if self.word("On", flag::TURN_ON) {
            return;
        }
        if self.next_is("Off") {
            self.pos += 1;
        }
    }

    /// Compile a calculation into `slot`.
    fn calc_into(&mut self, slot: u16, text: &str) {
        if text.is_empty() {
            return;
        }
        match compile(text, self.names) {
            Ok(calc) => self.step.set_calculation(slot, Some(&calc)),
            Err(message) => self.issue(message, Some(Part::Slot(slot))),
        }
    }

    /// A calculation alone, unless the next option is one of `stop`.
    fn bare(&mut self, slot: u16, stop: &[&str]) {
        if self.peek().is_some() && !self.stops(stop) {
            let text = self.take().expect("peeked");
            self.calc_into(slot, &text);
        }
    }

    /// `label: calculation`.
    fn labeled(&mut self, label: &str, slot: u16) {
        if let Some(text) = self.labeled_value(label) {
            self.calc_into(slot, &text);
        }
    }

    /// A field reference, `Table::Field`, with `[n]` or `[calculation]`.
    fn field_spec(&mut self, text: &str, rep_slot: Option<u16>, part: Part) -> Option<FieldSpec> {
        let (name, repetition) = split_repetition(text);
        let Some((table, field)) = name.split_once("::") else {
            self.issue(format!("“{text}” is not Table::Field"), Some(part));
            return None;
        };
        let found = self
            .names
            .find_occurrence(table)
            .require(&format!("the table occurrence “{table}”"))
            .and_then(|o| {
                let id = self
                    .names
                    .find_field(o, field)
                    .require(&format!("the field “{name}”"))?;
                Ok((o, id))
            });
        let (occurrence, id) = match found {
            Ok(found) => found,
            Err(reason) => {
                self.issue(reason, Some(part));
                return None;
            }
        };
        let mut rep = 1;
        if let Some(repetition) = repetition {
            match repetition.trim().parse::<i16>() {
                Ok(n) => rep = n,
                Err(_) => match rep_slot {
                    Some(slot) => {
                        rep = 0;
                        self.calc_into(slot, repetition);
                    }
                    None => self.issue(format!("“{repetition}” is not a repetition"), Some(part)),
                },
            }
        }
        Some(FieldSpec::new(id_key(occurrence)?, id_key(id)?, rep))
    }

    /// The inline field (`mReserved`), with a calculated repetition in slot
    /// 10.
    fn field(&mut self, stop: &[&str]) {
        if self.peek().is_none() || self.stops(stop) {
            return;
        }
        let text = self.take().expect("peeked");
        if let Some(spec) = self.field_spec(&text, Some(10), Part::Reserved) {
            self.step.set_field_spec(0, Some(spec));
        }
    }

    /// A field or variable target of slot `slot` from its text.
    fn target_text(&mut self, slot: u16, text: &str) {
        let text = text.trim();
        if text.starts_with('$') {
            let (name, repetition) = split_repetition(text);
            self.step.set_variable_target(slot, name);
            // A number is the spec's repetition; FileMaker shows a
            // calculated one only with spec repetition 0 [O: Save as XML].
            let rep = match repetition.map(|r| (r, r.trim().parse::<i16>())) {
                None => 1,
                Some((_, Ok(n))) if n >= 1 => n,
                Some((rep, _)) => {
                    self.calc_into(VARIABLE_NAME_SLOT + slot, rep);
                    0
                }
            };
            self.step
                .set_field_spec(slot, Some(FieldSpec::variable(rep)));
            return;
        }
        let part = if slot == 0 {
            Part::Reserved
        } else {
            Part::Slot(slot)
        };
        let rep_slot = (slot == 0).then_some(10);
        if let Some(spec) = self.field_spec(text, rep_slot, part) {
            self.step.set_field_spec(slot, Some(spec));
        }
    }

    /// `Target: x` (or the bare target when `label` is `None`).
    fn target(&mut self, label: Option<&str>, stop: &[&str]) {
        let text = match label {
            Some(label) => self.labeled_value(label),
            None if self.peek().is_some() && !self.stops(stop) => self.take(),
            None => None,
        };
        if let Some(text) = text {
            self.target_text(0, &text);
        }
    }

    /// A layout's text `“Name” (Occurrence)`: its key in `mReserved`.
    fn layout(&mut self, text: &str) {
        if text == UNKNOWN {
            self.issue(
                "The layout is not in this file".into(),
                Some(Part::Reserved),
            );
            return;
        }
        let Some((name, occurrence)) = quoted_remark(text) else {
            self.issue(
                format!("“{text}” is not a layout name"),
                Some(Part::Reserved),
            );
            return;
        };
        let what = match occurrence {
            Some(occurrence) => format!("the layout “{name}” of “{occurrence}”"),
            None => format!("the layout “{name}”"),
        };
        match self.names.find_layout(name, occurrence).require(&what) {
            Ok(id) => match id_key(id) {
                Some(key) => self.step.set_member_key(Some(key)),
                None => self.issue(
                    format!("The layout id {id} does not fit"),
                    Some(Part::Reserved),
                ),
            },
            Err(reason) => self.issue(reason, Some(Part::Reserved)),
        }
    }

    /// A script's text `“Name”` (or `… from file: “File”`).
    fn script(&mut self, text: &str) {
        if text == UNKNOWN || text.contains(" from file: ") {
            self.issue(
                "The script is not in this file".into(),
                Some(if text.contains(" from file: ") {
                    Part::Key(0x20)
                } else {
                    Part::Reserved
                }),
            );
            return;
        }
        let Some(name) = unquote(text) else {
            self.issue(
                format!("“{text}” is not a script name"),
                Some(Part::Reserved),
            );
            return;
        };
        let found = self
            .names
            .find_script(name)
            .require(&format!("the script “{name}”"));
        match found.map(serial_key) {
            Ok(Some(key)) => self.step.set_script_key(Some(key)),
            Ok(None) => self.issue(
                format!("The script id of “{name}” does not fit"),
                Some(Part::Reserved),
            ),
            Err(reason) => self.issue(reason, Some(Part::Reserved)),
        }
    }

    /// Perform Script on Server with Callback's callback from the list: the
    /// script's key as a quoted hex literal in slot 3, with `kOption`
    /// (script-steps.md 12.2). FileMaker's case for the digits a-f is not
    /// observed; upper case is written.
    fn callback_script(&mut self, text: &str) {
        let part = Some(Part::Slot(3));
        if text == UNKNOWN || text.contains(" from file: ") {
            self.issue("The callback script is not in this file".into(), part);
            return;
        }
        let Some(name) = unquote(text) else {
            self.issue(format!("“{text}” is not a script name"), part);
            return;
        };
        match self
            .names
            .find_script(name)
            .require(&format!("the script “{name}”"))
        {
            Ok(id) => {
                let hex: String = om_hbam::serial::encode(id)
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                let literal =
                    compile(&format!("\"{hex}\""), self.names).expect("a text literal compiles");
                self.step.set_calculation(3, Some(&literal));
                self.step.flags |= flag::OPTION;
            }
            Err(reason) => self.issue(reason, part),
        }
    }

    /// One of `labels` as the step value (`kMenuSelection`).
    fn choice(&mut self, labels: &[(u16, &str)]) -> bool {
        let Some(peek) = self.peek() else {
            return false;
        };
        let Some((value, _)) = labels
            .iter()
            .find(|(_, l)| l.eq_ignore_ascii_case(peek.trim()))
        else {
            return false;
        };
        self.step.value = *value;
        self.step.flags |= flag::MENU_SELECTION;
        self.pos += 1;
        true
    }

    /// The file path list `“path¶path”` at `[00 0, 82]`.
    fn paths_text(&mut self, text: &str) {
        let Some(list) = unquote(text) else {
            self.issue(
                format!("“{text}” is not a file path list"),
                Some(Part::Slot(0)),
            );
            return;
        };
        let stream = match path_stream(list) {
            Ok(stream) => stream,
            Err(reason) => {
                self.issue(reason, Some(Part::Slot(0)));
                return;
            }
        };
        self.step.set_slot_child(
            0,
            crate::StepData::PATH_STRING,
            Some(bytes_node(&[0x82], &stream)),
        );
        self.step.flags |= flag::FILE_PATH;
    }

    fn paths(&mut self) {
        if self.peek().is_some_and(|p| p.starts_with('“')) {
            let text = self.take().expect("peeked");
            self.paths_text(&text);
        }
    }

    fn create_folders(&mut self) {
        self.on_off("Create folders", flag::OPTION2);
    }

    fn open_and_email(&mut self) {
        self.word("Automatically open", flag::OPEN_AUTOMATICALLY);
        self.word("Create email", flag::CREATE_EMAIL);
    }

    /// `Current Window` or `Name: calculation`: a named window is step
    /// value 1, as all 36 of the corpus are; FileMaker shows the current
    /// window for 0 [O: Save as XML].
    fn window(&mut self) {
        if self.next_is("Current Window") {
            self.pos += 1;
        } else if let Some(text) = self.labeled_value("Name") {
            self.calc_into(0, &text);
            self.step.value = 1;
            self.step.flags |= flag::MENU_SELECTION;
        }
    }

    fn current_file_only(&mut self) {
        self.word("Current file", flag::LIMIT_TO_CURRENT_FILE);
    }

    fn animation(&mut self) {
        if let Some(name) = self.labeled_value("Animation") {
            match ANIMATIONS
                .iter()
                .position(|a| a.eq_ignore_ascii_case(&name))
            {
                Some(n) => self.step.set_list_value(6, Some(n as u32)),
                None => match name.parse::<u32>() {
                    Ok(n) => self.step.set_list_value(6, Some(n)),
                    Err(_) => {
                        self.issue(format!("“{name}” is not an animation"), Some(Part::Slot(6)))
                    }
                },
            }
        }
    }

    fn bounds(&mut self) {
        for (label, slot) in [("Height", 1), ("Width", 2), ("Top", 3), ("Left", 4)] {
            self.labeled(label, slot);
        }
    }

    /// The external data source `“Name”`: data sources are not resolved
    /// by name yet, so a name leaves the reference unset.
    fn data_source(&mut self, text: &str) {
        self.issue(
            format!("The data source {text} is not resolved"),
            Some(Part::Key(0x20)),
        );
    }
}

/// `text` split into its name and a trailing `[repetition]`.
fn split_repetition(text: &str) -> (&str, Option<&str>) {
    let text = text.trim();
    if let Some(stripped) = text.strip_suffix(']') {
        // The matching `[` of the final `]`.
        let mut depth = 0i32;
        for (i, c) in stripped.char_indices().rev() {
            match c {
                ']' => depth += 1,
                '[' if depth == 0 => return (&text[..i], Some(&stripped[i + 1..])),
                '[' => depth -= 1,
                _ => {}
            }
        }
    }
    (text, None)
}

/// The longest string a path list stores: its length prefix holds 14 bits.
pub(super) const MAX_PATH_BYTES: usize = 0x3fff;

/// A path list as FileMaker streams it (`UniversalPathListToStream`,
/// script-steps.md 11.0): a u32 count, then per path its kind, volume and
/// path as length-prefixed masked strings. `kind:path` lines; a path
/// without a kind has an empty one. A path longer than the prefix can say
/// ([`MAX_PATH_BYTES`] encoded bytes) is refused, not cut.
pub(super) fn path_stream(list: &str) -> Result<Vec<u8>, String> {
    let paths: Vec<&str> = list
        .split('¶')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = (paths.len() as u32).to_be_bytes().to_vec();
    let string = |out: &mut Vec<u8>, text: &str| {
        let bytes = om_scsu::encode_scsu(text);
        if bytes.len() > MAX_PATH_BYTES {
            return Err(format!(
                "A file path of {} bytes is longer than the {MAX_PATH_BYTES} a step stores",
                bytes.len()
            ));
        }
        if bytes.len() < 0x80 {
            out.push(bytes.len() as u8);
        } else {
            out.push(0x80 | (bytes.len() >> 8) as u8);
            out.push(bytes.len() as u8);
        }
        out.extend(bytes);
        Ok(())
    };
    for path in paths {
        let (kind, rest) = match path.split_once(':') {
            Some((kind, rest))
                if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic()) =>
            {
                (kind, rest)
            }
            _ => ("", path),
        };
        string(&mut out, kind)?;
        string(&mut out, "")?;
        string(&mut out, rest)?;
    }
    Ok(out)
}

/// Compile a step calculation against the file's names: the formatted
/// and Polish streams with no context occurrence (calc-encoding.md 1).
pub fn compile(text: &str, names: &dyn StepNames) -> Result<Calculation, String> {
    let ctx = om_formula::Context {
        decimal_comma: names.decimal_comma(),
        ..om_formula::Context::new(names.calc_names())
    };
    match om_formula::compile(text, &ctx) {
        Ok(compiled) => Ok(Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )),
        Err(error) => Err(format!(
            "“{text}” is not a valid calculation (error {})",
            error.code.number()
        )),
    }
}

/// The rules: one per formatter rule of `steps.rs`.
fn rule(p: &mut Parser<'_>) {
    let id = p.step.id();
    match id {
        1 => perform_script(p),
        62 => {
            if p.next_is("Indefinitely") {
                p.pos += 1;
            } else if let Some(text) = p.labeled_value("Duration (seconds)") {
                // A duration is choice 1 (corpus).
                p.step.value = 1;
                p.calc_into(0, &text);
            }
        }
        68 | 72 | 125 => p.bare(0, &[]),
        71 => {
            if let Some(flush) = p.labeled_value("Flush") {
                match flush.to_ascii_lowercase().as_str() {
                    "always" => {}
                    "minimum" => {
                        p.step.set_flag(flag::OPTION, true);
                        p.step.set_list_value(1, Some(2));
                    }
                    "defer" => {
                        p.step.set_flag(flag::OPTION, true);
                        p.step.set_list_value(1, Some(3));
                    }
                    _ => p.issue(
                        format!("“{flush}” is not a flush option"),
                        Some(Part::Slot(1)),
                    ),
                }
            }
        }
        85 | 86 | 94 | 115 | 168 | 223 => p.state(),
        87 => custom_dialog(p),
        103 => p.labeled("Text Result", 0),
        141 => set_variable(p),
        148 => {
            if let Some(text) = p.take() {
                p.script(&text);
            }
            p.labeled("Parameter", 0);
            p.labeled("Interval", 1);
        }
        164 | 210 => server_script(p),
        200 => {
            if !p.word("On", flag::OPTION) && p.next_is("Off") {
                p.pos += 1;
            }
            p.labeled("Custom debug info", 0);
        }
        205 => {
            p.word("Skip auto-enter options", flag::SELECT_ALL);
            p.word("Skip data entry validation", flag::OPTION);
            p.word("Override ESS locking conflicts", flag::OPTION2);
        }
        207 => {
            if let Some(text) = p.labeled_value("Condition") {
                p.step.set_flag(flag::OPTION, true);
                p.calc_into(0, &text);
            }
            if let Some(text) = p.labeled_value("Error Code") {
                p.step.set_flag(flag::OPTION2, true);
                p.calc_into(1, &text);
            }
            p.labeled("Error Message", 2);
        }
        208 => p.bare(0, &[]),
        6 => go_to_layout(p),
        16 | 99 => go_to_record(p),
        17 => {
            p.word("Select/perform", flag::SELECT_ALL);
            p.field(&[]);
        }
        74 => go_to_related_record(p),
        145 | 167 | 180 => {
            p.labeled("Object Name", 0);
            p.labeled("Repetition", 1);
        }
        228 => {
            p.labeled("List of record IDs", 0);
            if let Some(text) = p.labeled_value("Using layout") {
                if text != "<Current Layout>" {
                    p.layout(&text);
                }
            }
            p.word("New window", flag::OPTION);
            p.animation();
        }
        11 | 12 | 18 | 46 | 47 | 49 => {
            p.select();
            p.field(&[]);
        }
        45 => {
            p.choice(&[(1, "Undo"), (2, "Redo"), (3, "Toggle")]);
        }
        48 => {
            p.select();
            p.word("No style", flag::OPTION2);
            p.field(&[]);
        }
        128 => {
            p.dialog();
            p.bare(0, &[]);
            p.bare(1, &[]);
        }
        130 => {
            p.field(&["Start Position:", "End Position:"]);
            p.labeled("Start Position", 0);
            p.labeled("End Position", 1);
        }
        13 | 14 | 60 => {
            p.select();
            p.target(Some("Target"), &[]);
        }
        40 => {
            p.dialog();
            p.field(&[]);
        }
        61 => {
            p.select();
            p.target(Some("Target"), &[]);
            // Slot 0: FileMaker shows no other [O: Save as XML].
            if let Some(text) = p.take() {
                match unliteral(&text) {
                    Some(value) => p.step.set_text(0, Some(&value)),
                    None => p.issue(format!("{text} is not quoted text"), Some(Part::Slot(0))),
                }
            }
        }
        76 | 116 => {
            p.field(&[]);
            p.bare(0, &[]);
        }
        77 => {
            p.select();
            p.target(Some("Target"), &[]);
            p.bare(0, &[]);
        }
        91 => {
            p.dialog();
            p.field(&["Current contents", "Serial numbers"]);
            if p.next_is("Current contents") {
                p.pos += 1;
            } else if p.next_is("Serial numbers") {
                p.pos += 1;
                p.step.set_number(1, Some(2));
            } else if p.peek().is_some() {
                p.step.set_number(1, Some(3));
                p.bare(0, &[]);
            }
        }
        147 => {
            p.bare(1, &[]);
            p.bare(0, &[]);
        }
        131 => {
            if p.peek().is_some_and(|o| !o.starts_with('“')) {
                p.target(None, &[]);
            }
            p.paths();
        }
        132 => {
            p.field(&["Create folders:"]);
            p.paths();
            p.create_folders();
            p.open_and_email();
        }
        56 | 158 | 159 => {
            p.word("Reference", flag::STORE_REFERENCE);
            p.paths();
        }
        9 | 10 | 51 | 104 => p.dialog(),
        75 => {
            p.word("Skip data entry validation", flag::OPTION);
            p.dialog();
            p.word("Override ESS locking conflicts", flag::OPTION2);
        }
        35 | 139 => import_records(p),
        36 => {
            p.dialog();
            p.create_folders();
            p.paths();
            p.open_and_email();
        }
        143 => {
            p.word("Restore", flag::RESTORE);
            p.dialog();
            p.paths();
            // The records are slot 1, two bytes (script-steps.md 11.7).
            let records = p.peek().and_then(|w| {
                [(1, "Records being browsed"), (2, "Current record")]
                    .into_iter()
                    .find(|(_, l)| l.eq_ignore_ascii_case(w.trim()))
                    .map(|(v, _)| v)
            });
            if let Some(records) = records {
                p.step.set_number_u16(1, Some(records));
                p.pos += 1;
            }
            p.create_folders();
            p.open_and_email();
        }
        144 => save_records_as_pdf(p),
        63 => {
            // Through the email client (#836): the send method, the
            // dialog, To, Subject, Message and the attachment.
            if p.next_is("Send via E-mail Client") {
                p.pos += 1;
            }
            p.dialog();
            p.labeled("To", 0);
            p.labeled("Subject", 3);
            p.labeled("Message", 4);
            p.paths();
        }
        152 => {
            p.paths();
            p.choice(&[(1, "Records being browsed"), (2, "Current record")]);
            p.create_folders();
        }
        22 => {
            p.on_off("Pause", flag::PAUSE_SCRIPT);
            p.word("Restore", flag::RESTORE);
        }
        26 => {
            p.dialog();
            p.bare(0, &[]);
        }
        28 | 127 => {
            p.word("Restore", flag::RESTORE);
        }
        126 => {
            p.word("Restore", flag::RESTORE);
            p.word("Find without indexes", flag::OPTION);
        }
        39 | 42 | 43 => {
            p.word("Restore", flag::RESTORE);
            p.dialog();
        }
        150 => p.bare(0, &[]),
        154 => {
            p.choice(&[(1, "Ascending"), (2, "Descending"), (3, "Value list order")]);
            p.field(&[]);
        }
        155 => {
            p.choice(&[(1, "Replace"), (2, "Constrain"), (3, "Extend")]);
            p.field(&[]);
        }
        29 => {
            p.on_off("Lock", flag::LOCK);
            p.word("Include Edit Record Toolbar", flag::OPTION);
            p.choice(&[(1, "Show"), (2, "Hide"), (3, "Toggle")]);
        }
        30 => {
            p.choice(&[
                (1, "View as Form"),
                (2, "View as List"),
                (3, "View as Table"),
                (4, "Cycle"),
            ]);
        }
        31 => {
            p.choice(&[
                (1, "Resize to Fit"),
                (2, "Maximize"),
                (3, "Minimize"),
                (4, "Restore"),
                (5, "Hide"),
            ]);
        }
        41 | 55 => p.on_off("Pause", flag::PAUSE_SCRIPT),
        81 => {
            p.choice(&[
                (1, "Home"),
                (2, "End"),
                (3, "Page Up"),
                (4, "Page Down"),
                (5, "To Selection"),
            ]);
        }
        92 => {
            p.choice(&[(1, "Show"), (2, "Hide"), (3, "Toggle")]);
        }
        97 => {
            p.on_off("Lock", flag::LOCK);
            p.choice(&[
                (1, "25%"),
                (2, "50%"),
                (3, "75%"),
                (4, "100%"),
                (5, "150%"),
                (6, "200%"),
                (7, "300%"),
                (8, "400%"),
                (9, "Zoom In"),
                (10, "Zoom Out"),
            ]);
        }
        80 => {
            p.word("Flush cached join results", flag::OPTION);
            p.word("Flush cached external data", flag::OPTION2);
        }
        119 => {
            p.window();
            p.current_file_only();
            p.bounds();
        }
        120 => {
            p.choice(&[
                (1, "Tile Horizontally"),
                (2, "Tile Vertically"),
                (3, "Cascade Window"),
                (4, "Bring All to Front"),
            ]);
        }
        121 | 123 => {
            p.window();
            p.current_file_only();
        }
        122 => new_window(p),
        124 => {
            p.window();
            p.current_file_only();
            p.labeled("New Title", 1);
        }
        166 => {
            p.on_off("Lock", flag::LOCK);
            p.choice(&[(1, "Show"), (2, "Hide"), (3, "Toggle")]);
        }
        3 => {
            p.labeled("Window name", 0);
            if let Some(text) = p.labeled_value("Destination file") {
                p.paths_text(&text);
            }
            p.word("Include details for analysis tools", flag::OPTION);
            p.word(
                "Save each layout object's binary data under its node",
                flag::OPTION2,
            );
            if let Some(text) = p.labeled_value("Specify options as JSON") {
                p.step.flags |= flag::EXTENDED_DATA;
                p.calc_into(1, &text);
            }
        }
        33 => {
            p.on_off("Open hidden", flag::OPTION);
            if let Some(text) = p.take() {
                if text.starts_with('“') || text == UNKNOWN {
                    p.data_source(&text);
                } else {
                    p.calc_into(0, &text);
                }
            }
        }
        34 => {
            if let Some(text) = p.take() {
                if text == "Current File" {
                } else if text.starts_with('“') || text == UNKNOWN {
                    p.data_source(&text);
                } else {
                    p.calc_into(0, &text);
                }
            }
        }
        37 => {
            p.paths();
            p.choice(&[
                (1, "copy of current file"),
                (2, "compacted copy (smaller)"),
                (3, "clone (no records)"),
                (4, "self-contained copy (single file)"),
            ]);
            p.create_folders();
            p.open_and_email();
        }
        84 => {
            p.choice(&[(1, "On"), (2, "On (Hidden)"), (3, "Off")]);
        }
        95 => {
            p.dialog();
            p.paths();
        }
        96 => {
            p.labeled("Window name", 0);
            p.on_off("Replace UUIDs", flag::LINK_AVAIL);
        }
        188 | 189 | 191 => {
            p.paths();
            p.target(Some("Target"), &[]);
        }
        190 => {
            p.paths();
            p.create_folders();
        }
        192 => {
            p.labeled("File ID", 0);
            if let Some(text) = p.labeled_value("Data source") {
                p.target_text(0, &text);
            }
            p.choice(&[(1, "Write as: UTF-16"), (2, "Write as: UTF-8")]);
            p.word("Append line feed", flag::OPTION2);
        }
        193 => {
            p.labeled("File ID", 0);
            p.labeled("Amount (bytes)", 1);
            p.target(Some("Target"), &[]);
            p.choice(&[
                (1, "Read as: UTF-16"),
                (2, "Read as: UTF-8"),
                (3, "Read as: Bytes"),
            ]);
        }
        194 => {
            p.labeled("File ID", 0);
            p.target(Some("Target"), &[]);
        }
        195 => {
            p.labeled("File ID", 0);
            p.labeled("New position", 1);
        }
        196 => p.labeled("File ID", 0),
        197 => {
            if let Some(text) = p.labeled_value("Target file") {
                if !text.is_empty() {
                    p.paths_text(&text);
                }
            }
        }
        199 => {
            if let Some(text) = p.labeled_value("Source file") {
                if !text.is_empty() {
                    p.paths_text(&text);
                }
            }
            p.labeled("New name", 0);
        }
        134 => {
            p.labeled("Account Name", 0);
            p.labeled("Password", 1);
            p.word("Expire password", flag::OPTION);
        }
        135 => p.labeled("Account Name", 0),
        136 => {
            p.labeled("Account Name", 0);
            p.labeled("New Password", 1);
            p.word("Expire password", flag::OPTION);
        }
        137 => {
            p.labeled("Account Name", 0);
            if !p.word("Deactivate", flag::OPTION) && p.next_is("Activate") {
                p.pos += 1;
            }
        }
        138 => {
            p.dialog();
            p.labeled("Account Name", 0);
            p.labeled("Password", 1);
        }
        83 => {
            p.labeled("Old Password", 0);
            p.labeled("New Password", 1);
            p.dialog();
        }
        64 => {
            p.labeled("Service Name", 0);
            p.labeled("Topic", 1);
            p.labeled("Commands", 2);
        }
        65 => {
            p.dialog();
            p.bare(0, &[]);
        }
        66 => p.bare(0, &[]),
        67 => {
            if let Some(text) = p.take() {
                match unquote(&text) {
                    Some(script) => p.step.set_text(0, Some(script)),
                    None => p.calc_into(0, &text),
                }
            }
        }
        111 => {
            p.dialog();
            p.word("In external browser", flag::OPTION);
            p.bare(0, &[]);
        }
        117 => {
            p.dialog();
            if let Some(text) = p.labeled_value("Calculated SQL Text") {
                p.calc_into(0, &text);
            } else if let Some(text) = p.labeled_value("SQL Text") {
                p.step.set_text(0, Some(&text));
            }
        }
        142 => {
            if let Some(text) = p.take() {
                if text != "[Standard FileMaker Menus]" {
                    p.issue(
                        format!("The menu set {text} is not resolved"),
                        Some(Part::Reserved),
                    );
                }
            }
            p.on_off("Use as file default", flag::OPTION2);
        }
        146 => {
            p.labeled("Object Name", 0);
            for (value, action) in [
                (1, "Action: Reset"),
                (2, "Action: Reload"),
                (3, "Action: Go Forward"),
                (4, "Action: Go Back"),
            ] {
                if p.next_is(action) {
                    p.pos += 1;
                    p.step.value = value;
                    p.step.flags |= flag::MENU_SELECTION;
                }
            }
            p.labeled("URL", 1);
        }
        157 => p.field(&[]),
        160 => {
            p.select();
            p.dialog();
            p.target(Some("Target"), &[]);
            p.bare(
                0,
                &[
                    "Verify SSL Certificates",
                    "cURL options:",
                    "Do not automatically encode URL",
                ],
            );
            p.word("Verify SSL Certificates", flag::VERIFY_SSL_CERTIFICATES);
            p.labeled("cURL options", 1);
            p.word("Do not automatically encode URL", flag::OPTION);
        }
        174 => {
            p.choice(&[(1, "On"), (2, "Off"), (3, "Toggle")]);
        }
        175 => {
            p.labeled("Object Name", 0);
            p.labeled("Function Name", 1);
            if let Some(text) = p.labeled_value("Parameters") {
                let params = split_top(&text, ',');
                p.step.set_number(0, Some(params.len() as i32));
                for (n, param) in params.iter().enumerate() {
                    p.calc_into(10 + n as u16, param);
                }
            }
        }
        178 => {
            p.choice(&[(0, "Stopped"), (1, "Paused"), (2, "Playing")]);
        }
        181 => {
            p.word("Allow Folder Creation", flag::OPTION);
            if p.peek().is_some_and(|o| o.starts_with('$')) {
                let name = p.take().expect("peeked");
                p.step.set_text(0, Some(&name));
            }
            p.bare(3, &[]);
            p.bare(4, &[]);
        }
        182 => {
            p.dialog();
            if let Some(text) = p.labeled_value("Table") {
                if text != "<Current Table>" {
                    let table = match unquote(&text) {
                        Some(name) => p
                            .names
                            .find_table(name)
                            .require(&format!("the table {text}")),
                        None => Err(format!("{text} is not a table name")),
                    };
                    match table.map(id_key) {
                        // Step value 1 with the table, as the corpus' 42
                        // stores it; FileMaker shows none for 0 [O].
                        Ok(Some(key)) => {
                            p.step.set_member_key(Some(key));
                            p.step.value = 1;
                            p.step.flags |= flag::MENU_SELECTION;
                        }
                        Ok(None) => p.issue(
                            format!("The table id of {text} does not fit"),
                            Some(Part::Reserved),
                        ),
                        Err(reason) => p.issue(reason, Some(Part::Reserved)),
                    }
                }
            }
        }
        203 => {
            p.select();
            p.target(Some("Target"), &[]);
            p.bare(0, &[]);
        }
        211 => {
            p.labeled("Flow", 0);
            p.labeled("URL", 1);
            p.labeled("JSON Data", 2);
            p.target(Some("Target"), &[]);
        }
        69 | 237 | 247 => {}
        _ => generic(p),
    }
}

fn perform_script(p: &mut Parser<'_>) {
    match p.labeled_value("Specified").map(|s| s.to_ascii_lowercase()) {
        Some(s) if s == "by name" => by_name(p),
        _ => {
            if p.peek()
                .is_some_and(|o| label_value(o, "Parameter").is_none())
            {
                let text = p.take().expect("peeked");
                p.script(&text);
            }
        }
    }
    p.labeled("Parameter", 0);
}

/// "Specified: By name" of Perform Script and Perform Script on Server:
/// `[00 1]` = 1, the name calculation in slot 2 and the bits FileMaker
/// writes with them; without the bits it shows "From list" [O: Save as
/// XML].
fn by_name(p: &mut Parser<'_>) {
    p.step.set_list_value(1, Some(1));
    p.step.flags |= flag::STORE_REFERENCE | flag::RESTORE;
    if let Some(text) = p.take() {
        p.calc_into(2, &text);
    }
}

fn server_script(p: &mut Parser<'_>) {
    match p.labeled_value("Specified").map(|s| s.to_ascii_lowercase()) {
        Some(s) if s == "by name" => by_name(p),
        _ => {
            if p.peek()
                .is_some_and(|o| label_value(o, "Parameter").is_none())
            {
                let text = p.take().expect("peeked");
                p.script(&text);
            }
        }
    }
    p.labeled("Parameter", 0);
    if p.step.id() == 164 {
        if let Some(value) = p.labeled_value("Wait for completion") {
            p.step
                .set_flag(flag::OPTION, value.eq_ignore_ascii_case("off"));
        }
    } else {
        // Slots and flags of script-steps.md 12.2: by name, `[00 6]` = 1
        // and the name in slot 7 with `0x1000 | 0x2000`; the parameter in
        // slot 4 with `0x200`.
        let stop = ["Callback script parameter:", "State:"];
        match p.labeled_value("Callback script specified") {
            Some(how) if how.eq_ignore_ascii_case("by name") => {
                p.step.set_list_value(6, Some(1));
                p.step.flags |= flag::SELECT_ALL | flag::EXIT;
                p.bare(7, &stop);
            }
            Some(how)
                if how.eq_ignore_ascii_case("from list")
                    && p.peek().is_some()
                    && !p.stops(&stop) =>
            {
                let text = p.take().expect("peeked");
                p.callback_script(&text);
            }
            _ => {}
        }
        p.labeled("Callback script parameter", 4);
        if p.step.calculation(4).is_some() {
            p.step.flags |= flag::OPTION2;
        }
        p.choice(&[
            (1, "State: Halt"),
            (2, "State: Exit"),
            (3, "State: Resume"),
            (4, "State: Pause"),
        ]);
    }
}

fn custom_dialog(p: &mut Parser<'_>) {
    p.labeled("Title", 0);
    p.labeled("Message", 1);
    for (n, label) in (0u16..).zip(["Default Button", "Button 2", "Button 3"]) {
        let Some(text) = p.labeled_value(label) else {
            continue;
        };
        // `“OK”, Commit: “Yes”`.
        let (label_text, commit) = match text.rsplit_once(", Commit: ") {
            Some((label_text, commit)) => (label_text, unquote(commit).unwrap_or(commit)),
            None => (text.as_str(), "No"),
        };
        match unquote(label_text) {
            Some(t) => {
                // FileMaker 26 stores a label calculation beside the text.
                p.step.set_text(n, Some(t));
                let literal = format!("\"{}\"", t.replace('\\', "\\\\").replace('"', "\\\""));
                p.calc_into(5 + n, &literal);
            }
            None => p.calc_into(5 + n, label_text),
        }
        p.step
            .set_number_bool(3 + n, Some(commit.eq_ignore_ascii_case("yes")));
    }
    // The commit flags are stored for every button, shown or not, as one
    // byte each.
    for n in 0u16..3 {
        if p.step.number(3 + n).is_none() {
            p.step.set_number_bool(3 + n, Some(false));
        }
    }
    for n in 0u16..3 {
        let Some(text) = p.labeled_value(&format!("Input #{}", n + 1)) else {
            continue;
        };
        let mut parts = split_top(&text, ',');
        let password = parts.last().is_some_and(|l| l.trim() == "Password");
        if password {
            parts.pop();
            p.step.set_number_bool(n, Some(true));
        }
        if let Some(target) = parts.first() {
            p.target_text(n, target.trim());
        }
        if let Some(label) = parts.get(1) {
            p.calc_into(2 + n, label.trim());
        }
    }
}

fn set_variable(p: &mut Parser<'_>) {
    if p.peek().is_some_and(|o| label_value(o, "Value").is_none()) {
        let text = p.take().expect("peeked");
        let (name, repetition) = split_repetition(&text);
        p.step.set_text(0, Some(name));
        // FileMaker stores repetition 1 when none is given.
        p.calc_into(2, repetition.unwrap_or("1"));
    }
    p.labeled("Value", 1);
}

fn go_to_layout(p: &mut Parser<'_>) {
    if p.next_is("original layout") {
        p.pos += 1;
        p.step.value = 1;
        p.step.flags |= flag::MENU_SELECTION;
    } else if let Some(text) = p.labeled_value("Layout Name") {
        p.step.value = 3;
        p.step.flags |= flag::MENU_SELECTION;
        p.calc_into(5, &text);
    } else if let Some(text) = p.labeled_value("Layout Number") {
        p.step.value = 4;
        p.step.flags |= flag::MENU_SELECTION;
        p.calc_into(5, &text);
    } else if p
        .peek()
        .is_some_and(|o| label_value(o, "Animation").is_none())
    {
        let text = p.take().expect("peeked");
        p.step.value = 5;
        p.layout(&text);
    }
    p.animation();
}

fn go_to_record(p: &mut Parser<'_>) {
    let portal = p.step.id() == 99;
    if portal {
        p.on_off("Select", flag::SELECT_ALL);
    }
    let set = |p: &mut Parser<'_>, value: u16| {
        p.step.value = value;
        p.step.flags |= flag::MENU_SELECTION;
    };
    if p.next_is("First") {
        p.pos += 1;
        set(p, 1);
    } else if p.next_is("Last") {
        p.pos += 1;
        set(p, 2);
    } else if p.next_is("Previous") || p.next_is("Next") {
        let value = if p.next_is("Previous") { 3 } else { 4 };
        p.pos += 1;
        set(p, value);
        p.on_off("Exit after last", flag::EXIT);
    } else {
        set(p, 5);
        p.dialog();
        p.bare(0, &[]);
    }
}

fn go_to_related_record(p: &mut Parser<'_>) {
    p.word("Show only related records", flag::RESTORE);
    p.word("Match found set", flag::OPTION2);
    if let Some(text) = p.labeled_value("From table") {
        let occurrence = match quoted_head(&text) {
            Some(name) => p
                .names
                .find_occurrence(name)
                .require(&format!("the table occurrence “{name}”")),
            None => Err(format!("{text} is not a table occurrence name")),
        };
        match occurrence {
            Ok(o) => {
                let key = om_formula::Key::from_id(o).0;
                p.step
                    .set_slot_child(0, 0x11, Some(om_hbam::OwnedNode::leaf([0x11], key)));
            }
            Err(reason) => p.issue(reason, Some(Part::Slot(0))),
        }
    }
    if let Some(text) = p.labeled_value("Using layout") {
        if text == "<Current Layout>" {
            p.step.value = 1;
        } else {
            p.step.value = 5;
            p.layout(&text);
        }
    }
    p.word("New window", flag::TURN_ON);
    p.animation();
}

fn new_window(p: &mut Parser<'_>) {
    if let Some(style) = p.labeled_value("Style") {
        let word: i32 = match style.as_str() {
            "Document" => WindowOptions::DOCUMENT_WORD as i32,
            "Dialog" => WindowOptions::DIALOG_WORD as i32,
            "Card" => WindowOptions::CARD_WORD as i32,
            _ => {
                p.issue(
                    format!("“{style}” is not a window style"),
                    Some(Part::Slot(0)),
                );
                0
            }
        };
        if word != 0 {
            p.step.set_number(0, Some(word));
        }
    }
    p.labeled("Name", 0);
    if let Some(text) = p.labeled_value("Using layout") {
        p.layout(&text);
        // FileMaker shows the layout for step value 5 [O: Save as XML; 35
        // of the corpus' 41 New Window steps with a layout].
        if p.step.member_key().is_some() {
            p.step.value = 5;
        }
    }
    p.bounds();
}

fn import_records(p: &mut Parser<'_>) {
    if p.step.id() == 139 {
        p.word("Open file after conversion", flag::OPTION);
        p.word("Skip index creation", flag::OPTION2);
    }
    p.dialog();
    let source = if p.step.id() == 35 {
        p.labeled_value("Source")
    } else if p.peek().is_some_and(|o| o.starts_with('“')) {
        p.take()
    } else {
        None
    };
    if let Some(text) = source {
        p.paths_text(&text);
        // A file source is step value 1 (script-steps.md 11.3); FileMaker
        // shows no source without it [O: Save as XML].
        if p.step.id() == 35 {
            p.step.value = 1;
            p.step.flags |= flag::MENU_SELECTION;
        }
    }
    p.word("Verify SSL Certificates", flag::VERIFY_SSL_CERTIFICATES);
}

fn save_records_as_pdf(p: &mut Parser<'_>) {
    p.word("Restore", flag::RESTORE);
    let to = p.labeled_value("Save to").unwrap_or_else(|| "File".into());
    match to.as_str() {
        "Target" => {
            p.step.set_list_value(11, Some(2));
            p.target(Some("Target"), &[]);
        }
        "Currently open PDF" => p.step.set_list_value(11, Some(3)),
        _ => {
            p.paths();
            p.create_folders();
            p.dialog();
        }
    }
    p.choice(&[(1, "Records being browsed"), (2, "Current record")]);
    p.open_and_email();
}

/// The formatter's generic rule: each stored parameter by its schema name.
fn generic(p: &mut Parser<'_>) {
    use crate::params::ParamValue;
    use crate::schema::ParamKind;
    let Some(schema) = p.step.schema() else {
        return;
    };
    for param in schema
        .params
        .iter()
        .filter(|q| q.name != "Collapsed" && q.kind != ParamKind::Index)
    {
        if param.name == "With dialog" {
            p.dialog();
            continue;
        }
        match param.kind {
            ParamKind::Flag => {
                if p.next_is(&param.name) {
                    p.pos += 1;
                    if p.step
                        .set_parameter(&param.name, ParamValue::Bool(true))
                        .is_err()
                    {
                        p.issue(format!("{} is not stored", param.name), None);
                    }
                }
            }
            ParamKind::Calculation => {
                if let Some(text) = p.labeled_value(&param.name) {
                    match compile(&text, p.names) {
                        Ok(calc) => {
                            if p.step
                                .set_parameter(&param.name, ParamValue::Calculation(calc))
                                .is_err()
                            {
                                p.issue(format!("{} is not stored", param.name), None);
                            }
                        }
                        Err(message) => p.issue(message, None),
                    }
                }
            }
            ParamKind::Text => {
                if let Some(text) = p.labeled_value(&param.name) {
                    if p.step
                        .set_parameter(&param.name, ParamValue::Text(text))
                        .is_err()
                    {
                        p.issue(format!("{} is not stored", param.name), None);
                    }
                }
            }
            ParamKind::Number => {
                if let Some(text) = p.labeled_value(&param.name) {
                    match text.parse::<i64>() {
                        Ok(n)
                            if p.step
                                .set_parameter(&param.name, ParamValue::Number(n))
                                .is_ok() => {}
                        _ => p.issue(format!("{} is not a number", param.name), None),
                    }
                }
            }
            ParamKind::Choice => {
                if let Some(text) = p.labeled_value(&param.name) {
                    let choice = param
                        .choices()
                        .into_iter()
                        .find(|(_, l)| l.eq_ignore_ascii_case(&text))
                        .map(|(n, _)| n)
                        .or_else(|| text.parse().ok());
                    match choice {
                        Some(n)
                            if p.step
                                .set_parameter(&param.name, ParamValue::Choice(n))
                                .is_ok() => {}
                        _ => p.issue(format!("“{text}” is not a choice of {}", param.name), None),
                    }
                }
            }
            ParamKind::Field | ParamKind::Target
                if p.peek()
                    .is_some_and(|o| o.contains("::") || o.starts_with('$')) =>
            {
                let text = p.take().expect("peeked");
                p.target_text(0, &text);
            }
            ParamKind::Layout if p.peek().is_some_and(|o| o.starts_with('“') || o == UNKNOWN) => {
                let text = p.take().expect("peeked");
                p.layout(&text);
            }
            ParamKind::Script if p.peek().is_some_and(|o| o.starts_with('“') || o == UNKNOWN) => {
                let text = p.take().expect("peeked");
                p.script(&text);
            }
            ParamKind::FileReference if p.peek().is_some_and(|o| o.starts_with('“')) => {
                let text = p.take().expect("peeked");
                p.paths_text(&text);
            }
            _ => {}
        }
    }
}

/// A quoted literal as the formatter writes Insert Text: `"text"` with
/// `\"`, `\\` and `¶` for a return.
fn unliteral(text: &str) -> Option<String> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '¶' => out.push('\r'),
            c => out.push(c),
        }
    }
    Some(out)
}

/// `text` split at `sep` outside parentheses, brackets and quotes.
pub(super) fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut scan = super::text::Scanner::bracketed();
    for c in text.chars() {
        if c == sep && scan.at(1) {
            out.push(current.trim().to_owned());
            current.clear();
            continue;
        }
        scan.feed(c);
        current.push(c);
    }
    if !current.trim().is_empty() || !out.is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}
