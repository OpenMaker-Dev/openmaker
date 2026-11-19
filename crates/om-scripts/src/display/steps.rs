//! The text of each step id: its options in FileMaker's order with
//! FileMaker's labels (the Claris script step reference: each step's
//! Options section and examples). Steps without a rule here show their
//! stored parameters in schema order ([`generic`]).

use super::{
    calculation_text, field_text, quoted, Form, LineKind, OptionSpan, StepNames, COMMENT, UNKNOWN,
};
use crate::key::StoredKey;
use crate::params::ParamValue;
use crate::runtime::WindowOptions;
use crate::schema::{Location, ParamKind, SlotKind};
use crate::step::{flag, ScriptStep, StepData, VARIABLE_NAME_SLOT};
use crate::value::node_bytes;

/// The comment step, `# (comment)`.
const COMMENT_STEP: u16 = 89;

/// A step's kind, text (without the disabled prefix) and option spans.
pub(super) fn text(
    step: &ScriptStep,
    names: &dyn StepNames,
    form: Form,
) -> (LineKind, String, Vec<OptionSpan>) {
    if step.id() == COMMENT_STEP {
        let comment = step.text(0).unwrap_or_default();
        if comment.is_empty() {
            return (LineKind::Comment, String::new(), Vec::new());
        }
        let text = match form {
            Form::Display => format!("{COMMENT}{comment}"),
            // One step a line: a return in the comment is `¶`.
            Form::Text => format!("{COMMENT}{}", super::escape_comment(&comment)),
        };
        let span = OptionSpan {
            range: COMMENT.len()..text.len(),
            parameter: Some(0),
            calculation_slot: None,
        };
        return (LineKind::Comment, text, vec![span]);
    }
    let mut line = Line {
        step,
        names,
        form,
        parts: Vec::new(),
    };
    let bracketed = options(&mut line);
    let mut text = step.name();
    let mut spans = Vec::new();
    if bracketed {
        text.push_str(" [");
        for (n, (part, parameter, calculation_slot)) in line.parts.iter().enumerate() {
            text.push_str(if n == 0 { " " } else { " ; " });
            let start = text.len();
            text.push_str(part);
            spans.push(OptionSpan {
                range: start..text.len(),
                parameter: *parameter,
                calculation_slot: *calculation_slot,
            });
        }
        text.push_str(" ]");
    }
    (LineKind::Step, text, spans)
}

/// A step's options as they are being collected.
struct Line<'a> {
    step: &'a ScriptStep,
    names: &'a dyn StepNames,
    form: Form,
    /// Each option's text, schema parameter and specific calculation slot.
    parts: Vec<(String, Option<usize>, Option<u16>)>,
}

impl Line<'_> {
    /// The schema index of the first parameter named `name`.
    fn param(&self, name: &str) -> Option<usize> {
        self.step
            .schema()
            .and_then(|s| s.params.iter().position(|p| p.name == name))
    }

    fn push(&mut self, param: &str, text: impl Into<String>) {
        let index = self.param(param);
        self.parts.push((text.into(), index, None));
    }

    fn push_calculation(&mut self, param: &str, slot: u16, text: impl Into<String>) {
        let index = self.param(param);
        self.parts.push((text.into(), index, Some(slot)));
    }

    fn has(&self, bits: u32) -> bool {
        self.step.has_flag(bits)
    }

    /// `word` when `on`.
    fn word(&mut self, param: &str, word: &str, on: bool) {
        if on {
            self.push(param, word);
        }
    }

    /// `label: On` or `label: Off`.
    fn on_off(&mut self, param: &str, label: &str, on: bool) {
        self.push(param, format!("{label}: {}", if on { "On" } else { "Off" }));
    }

    /// `With dialog: On|Off`, the inverse of `kNoInteract`.
    fn dialog(&mut self) {
        let on = !self.has(flag::NO_INTERACT);
        self.on_off("With dialog", "With dialog", on);
    }

    /// `Select` for "Select entire contents".
    fn select(&mut self) {
        let on = self.has(flag::SELECT_ALL);
        self.word("Select entire contents", "Select", on);
    }

    /// `On`/`Off` of the on/off steps (`kTurnOn`).
    fn state(&mut self) {
        let on = self.has(flag::TURN_ON);
        self.push("State", if on { "On" } else { "Off" });
    }

    fn calc(&self, slot: u16) -> Option<String> {
        self.step
            .calculation(slot)
            .map(|c| calculation_text(&c, self.names))
    }

    /// The calculation alone, when stored.
    fn bare(&mut self, param: &str, slot: u16) {
        if let Some(text) = self.calc(slot) {
            self.push_calculation(param, slot, text);
        }
    }

    /// `label: calculation`, when stored.
    fn labeled(&mut self, param: &str, label: &str, slot: u16) {
        if let Some(text) = self.calc(slot) {
            self.push_calculation(param, slot, format!("{label}: {text}"));
        }
    }

    /// `label: calculation`, and the bare label when nothing is stored, as
    /// FileMaker shows `Exit Script [ Text Result: ]`.
    fn labeled_always(&mut self, param: &str, label: &str, slot: u16) {
        let part = match self.calc(slot) {
            Some(text) => format!("{label}: {text}"),
            None => format!("{label}:"),
        };
        self.push_calculation(param, slot, part);
    }

    /// A field spec with its repetition: `[n]` past the first, or the
    /// repetition calculation in `rep_slot` when the spec's repetition is 0.
    fn field_with_rep(&self, spec: crate::FieldSpec, rep_slot: Option<u16>) -> String {
        let mut text = field_text(&spec, self.names);
        if spec.repetition() == 0 {
            if let Some(rep) = rep_slot.and_then(|s| self.calc(s)) {
                text.push_str(&format!("[{rep}]"));
            }
        }
        text
    }

    /// The inline field (`mReserved`), with a repetition calculation in
    /// slot 10.
    fn field(&mut self, param: &str) {
        if let Some(spec) = self.step.field_spec().filter(crate::FieldSpec::names_field) {
            let text = self.field_with_rep(spec, Some(10));
            self.push(param, text);
        }
    }

    /// A field or variable target in slot `slot` (0 is `mReserved`): the
    /// variable's name with its repetition, or the field.
    fn target_text(&self, slot: u16) -> Option<String> {
        let spec = self
            .step
            .slot_field_spec(slot)
            .filter(|s| s.is_variable() || s.names_field())?;
        if spec.is_variable() {
            let name = self.step.text(VARIABLE_NAME_SLOT + slot)?;
            // As a field's: the calculation only with spec repetition 0
            // [O: FileMaker's Save as XML].
            let rep = match spec.repetition() {
                0 => self
                    .calc(VARIABLE_NAME_SLOT + slot)
                    .filter(|r| !r.trim().is_empty()),
                n if n > 1 => Some(n.to_string()),
                _ => None,
            };
            return Some(match rep {
                Some(rep) => format!("{name}[{rep}]"),
                None => name,
            });
        }
        Some(self.field_with_rep(spec, (slot == 0).then_some(10)))
    }

    /// `Target: x` (or the bare target when `label` is `None`).
    fn target(&mut self, param: &str, label: Option<&str>) {
        if let Some(text) = self.target_text(0) {
            match label {
                Some(label) => self.push(param, format!("{label}: {text}")),
                None => self.push(param, text),
            }
        }
    }

    fn layout_text(&self, id: Option<u64>) -> String {
        let Some(id) = id else {
            return UNKNOWN.into();
        };
        match self.names.layout_name(id) {
            Some(name) => match self.names.layout_occurrence_name(id) {
                Some(occurrence) => format!("{} ({occurrence})", quoted(name)),
                None => quoted(name),
            },
            None => UNKNOWN.into(),
        }
    }

    /// The layout of `mReserved`.
    fn member_layout(&self) -> String {
        self.layout_text(self.step.member_key().and_then(|k| k.id()))
    }

    /// The script of `mReserved`, with the file it is in when it is in
    /// another file (a file reference under `20`).
    fn script(&self) -> String {
        let name = self
            .step
            .script_id()
            .and_then(|id| self.names.script_name(id))
            .map(quoted);
        let file = self.data_source();
        match (name, file) {
            (Some(name), Some(file)) => format!("{name} from file: {file}"),
            (None, Some(file)) => format!("{UNKNOWN} from file: {file}"),
            (Some(name), None) => name,
            (None, None) => UNKNOWN.into(),
        }
    }

    /// The external data source under `20` (`[u8 len][serial]`).
    fn data_source(&self) -> Option<String> {
        let node = self.step.data_leaf(StepData::FILE_REFERENCES)?;
        let bytes = node_bytes(node)?;
        let (&len, segment) = bytes.split_first()?;
        let id = om_hbam::serial::decode(segment.get(..usize::from(len))?)?;
        Some(
            self.names
                .data_source_name(id)
                .map(quoted)
                .unwrap_or_else(|| UNKNOWN.into()),
        )
    }

    /// The step's file path list, quoted, paths separated by spaces.
    fn paths_text(&self) -> Option<String> {
        let text = crate::runtime::path_list(self.step)?;
        // A path entered without a prefix (`$file`) has an empty kind.
        let paths: Vec<&str> = text
            .split(['\r', '\n'])
            .map(str::trim)
            .map(|p| p.strip_prefix(':').unwrap_or(p))
            .filter(|p| !p.is_empty())
            .collect();
        // Several paths are separated by `¶`, as returns separate them in
        // the stored list.
        (!paths.is_empty()).then(|| quoted(&paths.join("¶")))
    }

    fn paths(&mut self, param: &str) {
        if let Some(text) = self.paths_text() {
            self.push(param, text);
        }
    }

    /// `Create folders: On|Off` (`kOption2`).
    fn create_folders(&mut self) {
        let on = self.has(flag::OPTION2);
        self.on_off("Create folders", "Create folders", on);
    }

    /// "Automatically open" and "Create email" of the file-writing steps.
    fn open_and_email(&mut self) {
        let open = self.has(flag::OPEN_AUTOMATICALLY);
        self.word("Automatically open", "Automatically open", open);
        let email = self.has(flag::CREATE_EMAIL);
        self.word("Create email", "Create email", email);
    }

    /// The step value's label from `labels` (`(value, label)`), if any.
    fn choice(&mut self, param: &str, labels: &[(u16, &str)]) {
        let value = self.step.value;
        if let Some((_, label)) = labels.iter().find(|(v, _)| *v == value) {
            self.push(param, *label);
        }
    }

    /// `Current Window` or `Name: calculation` of the window steps.
    fn window(&mut self) {
        match self.calc(0) {
            Some(name) => self.push_calculation("Window", 0, format!("Name: {name}")),
            None => self.push("Window", "Current Window"),
        }
    }

    fn current_file_only(&mut self) {
        let on = self.has(flag::LIMIT_TO_CURRENT_FILE);
        self.word("Current file only", "Current file", on);
    }

    /// `Animation: name` when a transition is stored (slot 6 list value).
    fn animation(&mut self) {
        if let Some(value) = self.step.list_value(6) {
            self.push("Animation", format!("Animation: {}", animation_name(value)));
        }
    }

    /// The bounds of the window steps: height, width, top, left in slots
    /// 1-4.
    fn bounds(&mut self, param: &str) {
        for (label, slot) in [("Height", 1), ("Width", 2), ("Top", 3), ("Left", 4)] {
            self.labeled(param, label, slot);
        }
    }
}

/// A Go to Layout transition (`slot 6` list value). FileMaker Go's names
/// (the Claris reference); only `None` (0) is in the corpus, so the order
/// of the others is OpenMaker's (script-steps.md, step text).
fn animation_name(value: u32) -> String {
    const NAMES: [&str; 11] = [
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
    NAMES
        .get(value as usize)
        .map_or_else(|| format!("{value}"), |n| (*n).to_owned())
}

/// Collect the step's options; `false` when the step shows no brackets.
fn options(l: &mut Line<'_>) -> bool {
    let s = l.step;
    match s.id() {
        // Control.
        1 => perform_script(l),
        62 => {
            if l.step.calculation(0).is_some() {
                l.labeled("Duration (seconds)", "Duration (seconds)", 0);
            } else {
                l.push("Pause", "Indefinitely");
            }
        }
        68 | 72 | 125 => l.bare("Condition", 0),
        69 => return false,
        71 => {
            let flush = if l.has(flag::OPTION) {
                match s.list_value(1) {
                    Some(2) => "Minimum",
                    Some(3) => "Defer",
                    _ => "Always",
                }
            } else {
                "Always"
            };
            l.push("Flush", format!("Flush: {flush}"));
        }
        85 | 86 | 94 | 115 | 168 | 223 => l.state(),
        87 => custom_dialog(l),
        103 => l.labeled_always("Text result", "Text Result", 0),
        141 => set_variable(l),
        148 => {
            let script = l.script();
            l.push("Script", script);
            l.labeled("Parameter", "Parameter", 0);
            l.labeled("Interval (seconds)", "Interval", 1);
        }
        164 | 210 => server_script(l),
        200 => {
            let on = l.has(flag::OPTION);
            l.push("Enabled", if on { "On" } else { "Off" });
            l.labeled("Custom debug info", "Custom debug info", 0);
        }
        205 => {
            let auto = l.has(flag::SELECT_ALL);
            l.word("Skip auto-enter options", "Skip auto-enter options", auto);
            let validation = l.has(flag::OPTION);
            l.word(
                "Skip data entry validation",
                "Skip data entry validation",
                validation,
            );
            let ess = l.has(flag::OPTION2);
            l.word(
                "Override ESS locking conflicts",
                "Override ESS locking conflicts",
                ess,
            );
        }
        207 => {
            if l.has(flag::OPTION) {
                l.labeled_always("Condition", "Condition", 0);
            }
            if l.has(flag::OPTION2) {
                l.labeled_always("Error code", "Error Code", 1);
                l.labeled("Error message", "Error Message", 2);
            }
        }
        208 => l.bare("Session identifier", 0),
        // Navigation.
        6 => go_to_layout(l),
        16 | 99 => go_to_record(l),
        17 => {
            let on = l.has(flag::SELECT_ALL);
            l.word("Select/perform", "Select/perform", on);
            l.field("Field");
        }
        74 => go_to_related_record(l),
        145 | 167 | 180 => {
            l.labeled_always("Object name", "Object Name", 0);
            l.labeled("Repetition", "Repetition", 1);
        }
        228 => {
            // Slot 7, and the new window under `kTurnOn` with its name in
            // slot 0, which Pro keeps when it is turned off (script-steps.md
            // 12.2).
            l.labeled_always("List of record IDs", "List of record IDs", 7);
            let layout = if s.member_key().is_some() {
                l.member_layout()
            } else {
                "<Current Layout>".into()
            };
            l.push("Layout", format!("Using layout: {layout}"));
            let new_window = l.has(flag::TURN_ON);
            l.word("New window", "New window", new_window);
            l.animation();
        }
        // Editing.
        11 | 12 | 18 | 46 | 47 | 49 => {
            l.select();
            l.field("Field");
        }
        45 => l.choice("Undo/Redo", &[(1, "Undo"), (2, "Redo"), (3, "Toggle")]),
        48 => {
            l.select();
            let no_style = l.has(flag::OPTION2);
            l.word("No style", "No style", no_style);
            l.field("Field");
        }
        128 => {
            l.dialog();
            l.bare("Find/Replace", 0);
            l.bare("Find/Replace", 1);
        }
        130 => {
            l.field("Field");
            l.labeled("Start position", "Start Position", 0);
            l.labeled("End position", "End Position", 1);
        }
        // Fields.
        13 | 14 | 60 => {
            l.select();
            l.target("Target", Some("Target"));
        }
        40 => {
            l.dialog();
            l.field("Field");
        }
        61 => {
            l.select();
            l.target("Target", Some("Target"));
            if let Some(text) = s.text(1).or_else(|| s.text(0)) {
                let mut out = String::new();
                literal(&text, l.form, &mut out);
                l.push("Text", out);
            }
        }
        76 => {
            l.field("Field");
            l.bare("Value", 0);
        }
        77 => {
            l.select();
            l.target("Target", Some("Target"));
            l.bare("Value", 0);
        }
        91 => replace_field_contents(l),
        116 => {
            l.field("Field");
            l.bare("Value", 0);
        }
        147 => {
            l.bare("Target field by name", 1);
            l.bare("Value", 0);
        }
        131 => {
            l.target("Target", None);
            l.paths("File paths");
        }
        132 => {
            l.field("Field");
            l.paths("File paths");
            l.create_folders();
            l.open_and_email();
        }
        56 | 158 | 159 => {
            let reference = l.has(flag::STORE_REFERENCE);
            l.word("Store only a reference", "Reference", reference);
            l.paths("File paths");
        }
        // Records.
        9 | 10 | 51 | 104 => l.dialog(),
        75 => {
            let validation = l.has(flag::OPTION);
            l.word(
                "Skip data entry validation",
                "Skip data entry validation",
                validation,
            );
            l.dialog();
            let ess = l.has(flag::OPTION2);
            l.word(
                "Override ESS locking conflicts",
                "Override ESS locking conflicts",
                ess,
            );
        }
        35 | 139 => import_records(l),
        36 => {
            l.dialog();
            l.create_folders();
            l.paths("File paths");
            l.open_and_email();
        }
        143 => {
            let restore = l.has(flag::RESTORE);
            l.word("Restore", "Restore", restore);
            l.dialog();
            l.paths("File paths");
            // The records are slot 1 (two bytes), not the step value, which
            // is 0 in every corpus step (script-steps.md 11.7).
            match l.step.number(1) {
                Some(1) => l.push("Excel options", "Records being browsed"),
                Some(2) => l.push("Excel options", "Current record"),
                _ => {}
            }
            l.create_folders();
            l.open_and_email();
        }
        144 => save_records_as_pdf(l),
        63 => return send_mail(l),
        152 => {
            l.paths("File paths");
            l.choice(
                "Records",
                &[(1, "Records being browsed"), (2, "Current record")],
            );
            l.create_folders();
        }
        // Found sets.
        22 => {
            let pause = l.has(flag::PAUSE_SCRIPT);
            l.on_off("Pause", "Pause", pause);
            let restore = l.has(flag::RESTORE);
            l.word("Restore", "Restore", restore);
        }
        26 => {
            l.dialog();
            l.bare("Number of records", 0);
        }
        28 | 127 => {
            let restore = l.has(flag::RESTORE);
            l.word("Restore", "Restore", restore);
        }
        126 => {
            let restore = l.has(flag::RESTORE);
            l.word("Restore", "Restore", restore);
            let indexes = l.has(flag::OPTION);
            l.word("Find without indexes", "Find without indexes", indexes);
        }
        39 | 42 | 43 => {
            let restore = l.has(flag::RESTORE);
            l.word("Restore", "Restore", restore);
            l.dialog();
        }
        150 => l.bare("Find text", 0),
        154 => {
            l.choice(
                "Sort direction",
                &[(1, "Ascending"), (2, "Descending"), (3, "Value list order")],
            );
            l.field("Field");
        }
        155 => {
            l.choice("Find", &[(1, "Replace"), (2, "Constrain"), (3, "Extend")]);
            l.field("Field");
        }
        // Windows.
        29 => {
            let lock = l.has(flag::LOCK);
            l.on_off("Lock", "Lock", lock);
            let edit = l.has(flag::OPTION);
            l.word(
                "Include Edit Record Toolbar",
                "Include Edit Record Toolbar",
                edit,
            );
            l.choice("Show/Hide", &[(1, "Show"), (2, "Hide"), (3, "Toggle")]);
        }
        30 => l.choice(
            "View As",
            &[
                (1, "View as Form"),
                (2, "View as List"),
                (3, "View as Table"),
                (4, "Cycle"),
            ],
        ),
        31 => l.choice(
            "Adjust Window",
            &[
                (1, "Resize to Fit"),
                (2, "Maximize"),
                (3, "Minimize"),
                (4, "Restore"),
                (5, "Hide"),
            ],
        ),
        41 | 55 => {
            let pause = l.has(flag::PAUSE_SCRIPT);
            l.on_off("Pause", "Pause", pause);
        }
        81 => l.choice(
            "Scroll",
            &[
                (1, "Home"),
                (2, "End"),
                (3, "Page Up"),
                (4, "Page Down"),
                (5, "To Selection"),
            ],
        ),
        92 => l.choice("Text ruler", &[(1, "Show"), (2, "Hide"), (3, "Toggle")]),
        97 => {
            let lock = l.has(flag::LOCK);
            l.on_off("Lock", "Lock", lock);
            l.choice(
                "Zoom",
                &[
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
                ],
            );
        }
        80 => {
            let join = l.has(flag::OPTION);
            l.word(
                "Flush cached join results",
                "Flush cached join results",
                join,
            );
            let external = l.has(flag::OPTION2);
            l.word(
                "Flush cached external data",
                "Flush cached external data",
                external,
            );
        }
        119 => {
            l.window();
            l.current_file_only();
            l.bounds("Bounds");
        }
        120 => l.choice(
            "Arrange",
            &[
                (1, "Tile Horizontally"),
                (2, "Tile Vertically"),
                (3, "Cascade Window"),
                (4, "Bring All to Front"),
            ],
        ),
        121 | 123 => {
            l.window();
            l.current_file_only();
        }
        122 => new_window(l),
        124 => {
            l.window();
            l.current_file_only();
            l.labeled("New title", "New Title", 1);
        }
        166 => {
            let lock = l.has(flag::LOCK);
            l.on_off("Lock", "Lock", lock);
            l.choice("Menubar", &[(1, "Show"), (2, "Hide"), (3, "Toggle")]);
        }
        // Files.
        3 => {
            l.labeled_always("Window name", "Window name", 0);
            if let Some(paths) = l.paths_text() {
                l.push("File paths", format!("Destination file: {paths}"));
            }
            let details = l.has(flag::OPTION);
            l.word(
                "Include details for analysis tools",
                "Include details for analysis tools",
                details,
            );
            let binary = l.has(flag::OPTION2);
            l.word(
                "Save each layout object's binary data under its node",
                "Save each layout object's binary data under its node",
                binary,
            );
            if l.has(flag::EXTENDED_DATA) {
                l.labeled_always("Specify options as JSON", "Specify options as JSON", 1);
            }
        }
        33 => {
            let hidden = l.has(flag::OPTION);
            l.on_off("Open hidden", "Open hidden", hidden);
            let source = l.data_source().or_else(|| l.calc(0));
            if let Some(source) = source {
                l.push("Data source", source);
            }
        }
        34 => {
            let source = l
                .data_source()
                .or_else(|| l.calc(0))
                .unwrap_or_else(|| "Current File".into());
            l.push("Data source", source);
        }
        37 => {
            l.paths("File paths");
            l.choice(
                "Save a copy as",
                &[
                    (1, "copy of current file"),
                    (2, "compacted copy (smaller)"),
                    (3, "clone (no records)"),
                    (4, "self-contained copy (single file)"),
                ],
            );
            l.create_folders();
            l.open_and_email();
        }
        84 => l.choice(
            "Network access",
            &[(1, "On"), (2, "On (Hidden)"), (3, "Off")],
        ),
        95 => {
            l.dialog();
            l.paths("File paths");
        }
        96 => {
            l.labeled_always("Package name", "Window name", 0);
            let replace = l.has(flag::LINK_AVAIL);
            l.on_off("Replace UUIDs", "Replace UUIDs", replace);
        }
        188 | 189 | 191 => {
            l.paths("File paths");
            l.target("Target", Some("Target"));
        }
        190 => {
            l.paths("File paths");
            l.create_folders();
        }
        192 => {
            l.labeled_always("File ID", "File ID", 0);
            if let Some(target) = l.target_text(0) {
                l.push("Target", format!("Data source: {target}"));
            }
            l.choice(
                "Write as",
                &[(1, "Write as: UTF-16"), (2, "Write as: UTF-8")],
            );
            let line_feed = l.has(flag::OPTION2);
            l.word("Append line feed", "Append line feed", line_feed);
        }
        193 => {
            l.labeled_always("File ID", "File ID", 0);
            l.labeled_always("Amount", "Amount (bytes)", 1);
            l.target("Target", Some("Target"));
            l.choice(
                "Read as",
                &[
                    (1, "Read as: UTF-16"),
                    (2, "Read as: UTF-8"),
                    (3, "Read as: Bytes"),
                ],
            );
        }
        194 => {
            l.labeled_always("File ID", "File ID", 0);
            l.target("Target", Some("Target"));
        }
        195 => {
            l.labeled_always("File ID", "File ID", 0);
            l.labeled_always("New position", "New position", 1);
        }
        196 => l.labeled_always("File ID", "File ID", 0),
        197 => {
            let paths = l.paths_text();
            l.push("File paths", labeled_text("Target file", paths));
        }
        199 => {
            let paths = l.paths_text();
            l.push("File paths", labeled_text("Source file", paths));
            l.labeled_always("New name", "New name", 0);
        }
        // Accounts.
        134 => {
            l.labeled("Account name", "Account Name", 0);
            l.labeled("Password", "Password", 1);
            let expire = l.has(flag::OPTION);
            l.word("Expire password", "Expire password", expire);
        }
        135 => l.labeled_always("Account name", "Account Name", 0),
        136 => {
            l.labeled_always("Account name", "Account Name", 0);
            l.labeled("New password", "New Password", 1);
            let expire = l.has(flag::OPTION);
            l.word("Expire password", "Expire password", expire);
        }
        137 => {
            l.labeled_always("Account name", "Account Name", 0);
            let off = l.has(flag::OPTION);
            l.push(
                "Activate/Deactivate",
                if off { "Deactivate" } else { "Activate" },
            );
        }
        138 => {
            l.dialog();
            l.labeled("Account name", "Account Name", 0);
            l.labeled("Password", "Password", 1);
        }
        83 => {
            l.labeled("Old password", "Old Password", 0);
            l.labeled("New password", "New Password", 1);
            l.dialog();
        }
        // Miscellaneous.
        64 => {
            l.labeled("Service", "Service Name", 0);
            l.labeled("Topic", "Topic", 1);
            l.labeled("Command", "Commands", 2);
        }
        65 => {
            l.dialog();
            l.bare("Phone number", 0);
        }
        66 => l.bare("Text", 0),
        67 => match l.calc(0) {
            Some(calc) => l.push("AppleScript", calc),
            None => {
                if let Some(text) = s.text(0) {
                    l.push("AppleScript", quoted(&text));
                }
            }
        },
        111 => {
            l.dialog();
            let external = l.has(flag::OPTION);
            l.word("In external browser", "In external browser", external);
            l.bare("URL", 0);
        }
        117 => {
            l.dialog();
            match l.calc(0) {
                Some(calc) => l.push("SQL query", format!("Calculated SQL Text: {calc}")),
                None => {
                    if let Some(text) = s.text(0) {
                        l.push("SQL query", format!("SQL Text: {text}"));
                    }
                }
            }
        }
        142 => {
            let name = match s.member_key() {
                Some(key) => menu_set(l, key),
                None => "[Standard FileMaker Menus]".into(),
            };
            l.push("Menu set", name);
            let default = l.has(flag::OPTION2);
            l.on_off("Use as file default", "Use as file default", default);
        }
        146 => {
            l.labeled_always("Object name", "Object Name", 0);
            match s.value {
                1 => l.push("Action", "Action: Reset"),
                2 => l.push("Action", "Action: Reload"),
                3 => l.push("Action", "Action: Go Forward"),
                4 => l.push("Action", "Action: Go Back"),
                _ => l.labeled("Action", "URL", 1),
            }
        }
        157 => l.field("Field"),
        160 => {
            l.select();
            l.dialog();
            l.target("Target", Some("Target"));
            l.bare("URL", 0);
            let verify = l.has(flag::VERIFY_SSL_CERTIFICATES);
            l.word("Verify SSL Certificates", "Verify SSL Certificates", verify);
            l.labeled("cURL options", "cURL options", 1);
            let raw = l.has(flag::OPTION);
            l.word(
                "Do not automatically encode URL",
                "Do not automatically encode URL",
                raw,
            );
        }
        174 => l.choice("Keyboard", &[(1, "On"), (2, "Off"), (3, "Toggle")]),
        175 => {
            l.labeled_always("Object name", "Object Name", 0);
            l.labeled_always("Function name", "Function Name", 1);
            let count = s.number(0).unwrap_or(0).clamp(0, 100) as u16;
            let params: Vec<String> = (0..count).filter_map(|i| l.calc(10 + i)).collect();
            if !params.is_empty() {
                l.push("Parameters", format!("Parameters: {}", params.join(", ")));
            }
        }
        178 => l.choice(
            "Playback state",
            &[(0, "Stopped"), (1, "Paused"), (2, "Playing")],
        ),
        181 => {
            let create = l.has(flag::OPTION);
            l.word("Allow Folder Creation", "Allow Folder Creation", create);
            if let Some(name) = s.text(0) {
                l.push("Target variable", name);
            }
            l.bare("Dialog title", 3);
            l.bare("Default location", 4);
        }
        182 => {
            l.dialog();
            // A base table key: its two-byte id from `0x8000`.
            let table = match s.member_key() {
                Some(key) => key
                    .id()
                    .and_then(|id| l.names.table_name(id))
                    .map_or_else(|| UNKNOWN.into(), quoted),
                None => "<Current Table>".into(),
            };
            l.push("Table", format!("Table: {table}"));
        }
        203 => {
            l.select();
            l.target("Target", Some("Target"));
            l.bare("Request", 0);
        }
        211 => {
            l.labeled("Flow", "Flow", 0);
            l.labeled("Flow", "URL", 1);
            l.labeled("Flow", "JSON Data", 2);
            l.target("Target", Some("Target"));
        }
        237 | 247 => return false,
        _ => return generic(l),
    }
    true
}

fn menu_set(l: &Line<'_>, key: StoredKey) -> String {
    key.id()
        .and_then(|id| l.names.menu_set_name(id))
        .map_or_else(|| UNKNOWN.into(), quoted)
}

/// Perform Script: `Specified: From list ; “Script” ; Parameter: calc` or
/// `Specified: By name ; calc ; Parameter: calc`.
fn perform_script(l: &mut Line<'_>) {
    if l.step.list_value(1) == Some(1) {
        l.push("Specify by name", "Specified: By name");
        let name = l.calc(2).unwrap_or_default();
        l.push_calculation("Specify by name", 2, name);
    } else {
        l.push("Specify by name", "Specified: From list");
        let script = l.script();
        l.push("Script", script);
    }
    l.labeled_always("Parameter", "Parameter", 0);
}

/// Perform Script on Server (with Callback): as Perform Script, then
/// `Wait for completion: On|Off`, or the callback script and its state.
fn server_script(l: &mut Line<'_>) {
    // PSOS uses Perform Script's canonical slot-2 name form. Keep
    // reading the old slot-1 form; callback steps use that slot too.
    let canonical = l.step.id() == 164 && l.step.list_value(1) == Some(1);
    let by_name = if canonical {
        Some(l.calc(2).unwrap_or_default())
    } else {
        l.calc(1)
    };
    match by_name {
        Some(name) if canonical => {
            l.push("Specify by name", "Specified: By name");
            l.push_calculation("Specify by name", 2, name);
        }
        Some(name) => {
            l.push("Script", "Specified: By name");
            l.push("Script", name);
        }
        None => {
            l.push("Script", "Specified: From list");
            let script = l.script();
            l.push("Script", script);
        }
    }
    l.labeled_always("Parameter", "Parameter", 0);
    if l.step.id() == 164 {
        let wait = !l.has(flag::OPTION);
        l.on_off("Wait for completion", "Wait for completion", wait);
    } else {
        match l.calc(2) {
            Some(name) => {
                l.push("Callback script", "Callback script specified: By name");
                l.push("Callback script", name);
            }
            None => l.push("Callback script", "Callback script specified: From list"),
        }
        l.labeled("Callback script", "Callback script parameter", 3);
        l.choice(
            "State",
            &[
                (1, "State: Halt"),
                (2, "State: Exit"),
                (3, "State: Resume"),
                (4, "State: Pause"),
            ],
        );
    }
}

/// Show Custom Dialog (script-steps.md 11.8): title, message, each button
/// with its commit option, each input field with its label.
fn custom_dialog(l: &mut Line<'_>) {
    l.labeled("Title", "Title", 0);
    l.labeled("Message", "Message", 1);
    let buttons = ["Default Button", "Button 2", "Button 3"];
    for (n, label) in (0u16..).zip(buttons) {
        // FileMaker 26 stores a label calculation beside the text label.
        let text = l.step.text(n).map(|t| quoted(&t)).or_else(|| l.calc(5 + n));
        if let Some(text) = text {
            let commit = l.step.number(3 + n) == Some(1);
            let commit = if commit { "Yes" } else { "No" };
            l.push(
                "Button labels",
                format!("{label}: {text}, Commit: {}", quoted(commit)),
            );
        }
    }
    for n in 0u16..3 {
        let Some(target) = l.target_text(n) else {
            continue;
        };
        let mut text = format!("Input #{}: {target}", n + 1);
        if let Some(label) = l.calc(2 + n) {
            text.push_str(&format!(", {label}"));
        }
        if l.step.number(n) == Some(1) {
            text.push_str(", Password");
        }
        l.push("Input fields", text);
    }
}

/// `label: text`, or `label:` when there is none.
fn labeled_text(label: &str, text: Option<String>) -> String {
    match text {
        Some(text) => format!("{label}: {text}"),
        None => format!("{label}:"),
    }
}

/// Set Variable: `$name[repetition] ; Value: calc`.
fn set_variable(l: &mut Line<'_>) {
    let mut name = l.step.text(0).unwrap_or_default();
    if let Some(rep) = l
        .calc(2)
        .filter(|r| r.trim() != "1" && !r.trim().is_empty())
    {
        name.push_str(&format!("[{rep}]"));
    }
    l.push("Name", name);
    l.labeled_always("Value", "Value", 1);
}

/// Go to Layout: the stored layout, `original layout` or a calculated
/// name or number, then the animation.
fn go_to_layout(l: &mut Line<'_>) {
    let step = l.step;
    match step.value {
        1 => l.push("Layout", "original layout"),
        3 => l.labeled_always("Layout", "Layout Name", 5),
        4 => l.labeled_always("Layout", "Layout Number", 5),
        _ => {
            let layout = l.member_layout();
            l.push("Layout", layout);
        }
    }
    l.animation();
}

/// Go to Record/Request/Page and Go to Portal Row.
fn go_to_record(l: &mut Line<'_>) {
    let portal = l.step.id() == 99;
    if portal {
        let select = l.has(flag::SELECT_ALL);
        l.on_off("Select entire contents", "Select", select);
    }
    let param = if portal { "Row" } else { "Record/Request/Page" };
    match l.step.value {
        1 => l.push(param, "First"),
        2 => l.push(param, "Last"),
        3 | 4 => {
            let which = if l.step.value == 3 {
                "Previous"
            } else {
                "Next"
            };
            l.push(param, which);
            let exit = l.has(flag::EXIT);
            l.on_off("Exit after last", "Exit after last", exit);
        }
        _ => {
            l.dialog();
            let number = if portal { "Row number" } else { "Number" };
            l.bare(number, 0);
        }
    }
}

/// Go to Related Record (script-steps.md 11.8).
fn go_to_related_record(l: &mut Line<'_>) {
    let step = l.step;
    let only = l.has(flag::RESTORE);
    l.word("Options", "Show only related records", only);
    if only && l.has(flag::OPTION2) {
        l.push("Options", "Match found set");
    }
    let occurrence = step
        .data
        .as_ref()
        .and_then(|d| d.slot(0))
        .and_then(|slot| slot.child(0x11))
        .and_then(node_bytes)
        .and_then(|bytes| {
            let (&len, segment) = bytes.split_first()?;
            om_hbam::slot::be_uint(segment.get(..usize::from(len))?)
        });
    let table = occurrence
        .and_then(|o| l.names.calc_names().occurrence_name(o))
        .map_or_else(|| UNKNOWN.into(), quoted);
    l.push("Related table", format!("From table: {table}"));
    let layout = match step.value {
        1 => "<Current Layout>".into(),
        _ => l.member_layout(),
    };
    l.push("Layout", format!("Using layout: {layout}"));
    if l.has(flag::TURN_ON) {
        l.push("Options", "New window");
    }
    l.animation();
}

/// New Window: style, name, layout, bounds.
fn new_window(l: &mut Line<'_>) {
    // Only FileMaker's own words have a name, so the text reads back to
    // the same word.
    let style = match l.step.number(0).map(|word| word as u32) {
        Some(WindowOptions::DOCUMENT_WORD) => Some("Document"),
        Some(WindowOptions::DIALOG_WORD) => Some("Dialog"),
        Some(WindowOptions::CARD_WORD) => Some("Card"),
        _ => None,
    };
    if let Some(style) = style {
        l.push("Window style and options", format!("Style: {style}"));
    }
    l.labeled("Window name", "Name", 0);
    if l.step.member_key().is_some() {
        let layout = l.member_layout();
        l.push("Layout", format!("Using layout: {layout}"));
    }
    l.bounds("Bounds");
}

/// Replace Field Contents: the field, then current contents, serial
/// numbers or the calculation (script-steps.md 11.8).
fn replace_field_contents(l: &mut Line<'_>) {
    l.dialog();
    l.field("Field");
    match l.step.number(1) {
        Some(2) => l.push("Replace with", "Serial numbers"),
        Some(3) => l.bare("Replace with", 0),
        _ => l.push("Replace with", "Current contents"),
    }
}

/// Import Records and Convert File: dialog, source, Verify SSL.
fn import_records(l: &mut Line<'_>) {
    if l.step.id() == 139 {
        let open = l.has(flag::OPTION);
        l.word(
            "Open file after conversion",
            "Open file after conversion",
            open,
        );
        let skip = l.has(flag::OPTION2);
        l.word("Skip index creation", "Skip index creation", skip);
    }
    l.dialog();
    let source = l.data_source().or_else(|| l.paths_text());
    if let Some(source) = source {
        let param = "Data source";
        if l.step.id() == 35 {
            l.push(param, format!("Source: {source}"));
        } else {
            l.push(param, source);
        }
    }
    let verify = l.has(flag::VERIFY_SSL_CERTIFICATES);
    l.word("Verify SSL Certificates", "Verify SSL Certificates", verify);
}

/// Save Records as PDF, FileMaker 26's form: `Restore ; Save to: File ;
/// “path” ; Create folders ; With dialog ; records`.
/// Send Mail through the email client, as FileMaker Pro 26.0.3 shows it
/// (#836; Pro's reading of #837's file): `Send via E-mail Client`, the
/// dialog, To, Subject, Message and the attachment. In the workspace the
/// attachment is its file name without the `file:` kind, as Pro shows it;
/// the text form keeps the stored path, which reads back. A step with an
/// option that reading did not show (CC, BCC, one email per record, SMTP,
/// OAuth, an attachment of another kind or several, or one with a
/// typographic quote in its name) keeps the generic text.
fn send_mail(l: &mut Line<'_>) -> bool {
    let step = l.step;
    let list = crate::runtime::path_list(step).unwrap_or_default();
    let attachments: Vec<&str> = list
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    // Measured: To (calculation 0, its Collect addresses number 0),
    // Subject 3 and Message 4. Every other calculation (CC 1, BCC 2, SMTP
    // and OAuth 5-21) and number (Collect for CC and BCC 1-2, one email per
    // record 3, SMTP 4-6, OAuth 7-8) is unmeasured. An attachment with a
    // typographic quote would end its quoted name early.
    let read = (1..=21)
        .filter(|slot| !matches!(slot, 3 | 4))
        .all(|slot| step.calculation(slot).is_none())
        && (1..=8).all(|slot| step.number(slot).unwrap_or(0) == 0)
        && attachments.len() <= 1
        && attachments
            .iter()
            .all(|p| p.starts_with("file:") && !p.contains(['“', '”']));
    if !read {
        return generic(l);
    }
    l.push("Send Mail", "Send via E-mail Client");
    l.dialog();
    l.labeled("Send Mail", "To", 0);
    l.labeled("Send Mail", "Subject", 3);
    l.labeled("Send Mail", "Message", 4);
    if let Some(path) = attachments.first() {
        let shown = match l.form {
            Form::Display => path.strip_prefix("file:").unwrap_or(path),
            Form::Text => path,
        };
        l.push("Send Mail", quoted(shown));
    }
    true
}

fn save_records_as_pdf(l: &mut Line<'_>) {
    let restore = l.has(flag::RESTORE);
    l.word("Restore", "Restore", restore);
    let to = match l.step.list_value(11) {
        Some(2) => "Target",
        Some(3) => "Currently open PDF",
        _ => "File",
    };
    l.push("Source", format!("Save to: {to}"));
    match to {
        "File" => {
            l.paths("File paths");
            l.create_folders();
            l.dialog();
        }
        "Target" => l.target("Target", Some("Target")),
        _ => {}
    }
    l.choice(
        "PDF options",
        &[(1, "Records being browsed"), (2, "Current record")],
    );
    l.open_and_email();
}

/// A text option in quotes as a calculation literal shows (Insert Text).
/// The text form also escapes the typographic quotes, as every quote
/// character closes a calculation's literal when it is read back.
fn literal(text: &str, form: Form, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '“' | '”' | '„' | '‟' | '＂' if form == Form::Text => {
                out.push('\\');
                out.push(c);
            }
            '\r' => out.push('¶'),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Steps without a rule: each stored parameter in schema order, labelled
/// with its schema name; Booleans by their name when on (`With dialog:
/// On|Off` always). Brackets when the schema has parameters.
fn generic(l: &mut Line<'_>) -> bool {
    let step = l.step;
    let Some(schema) = step.schema() else {
        return false;
    };
    let shown: Vec<_> = schema
        .params
        .iter()
        .filter(|p| p.name != "Collapsed" && p.kind != ParamKind::Index)
        .collect();
    if shown.is_empty() {
        return false;
    }
    for p in shown {
        if p.name == "With dialog" {
            l.dialog();
            continue;
        }
        let param = step.parameter(&p.name);
        let value = param.as_ref().and_then(|p| p.value());
        let text = match (p.kind, value) {
            (ParamKind::Flag, Some(ParamValue::Bool(true))) => Some(p.name.clone()),
            (ParamKind::Calculation, Some(ParamValue::Calculation(c))) => {
                Some(format!("{}: {}", p.name, calculation_text(&c, l.names)))
            }
            (ParamKind::Text, Some(ParamValue::Text(t))) => Some(format!("{}: {t}", p.name)),
            (ParamKind::Number, Some(ParamValue::Number(n))) => Some(format!("{}: {n}", p.name)),
            (ParamKind::Choice, Some(ParamValue::Choice(c))) => {
                let label = p
                    .choices()
                    .into_iter()
                    .find(|(n, _)| *n == c)
                    .map_or_else(|| c.to_string(), |(_, label)| label);
                Some(format!("{}: {label}", p.name))
            }
            (ParamKind::Field, Some(_)) | (ParamKind::Target, Some(_)) => l.target_text(0),
            (ParamKind::Layout, Some(_)) => Some(l.member_layout()),
            (ParamKind::Script, Some(_)) => Some(l.script()),
            (ParamKind::FileReference, _) => l.data_source().or_else(|| l.paths_text()),
            _ => None,
        };
        if let Some(text) = text {
            let slot = (p.kind == ParamKind::Calculation)
                .then(|| {
                    p.storage.iter().find_map(|location| match location {
                        Location::Slot {
                            slot,
                            kind: SlotKind::Calculation,
                        } => Some(*slot),
                        _ => None,
                    })
                })
                .flatten();
            match slot {
                Some(slot) => l.push_calculation(&p.name, slot, text),
                None => l.push(&p.name, text),
            }
        }
    }
    true
}
