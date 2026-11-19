//! The script oracle's batteries (`tools/fms-script-harness/batteries`):
//! probe scripts written as FileMaker-style step lines in `== Name` blocks
//! and `@trigger` lines, then run lines with `@call`/`@watch` directives
//! after `--- run`. Each
//! probe becomes an `om_scripts::Script` built with the codec's edit API, the
//! way `make_patch.py` and FileMaker's XML importer store it, with its block
//! indexes computed as FileMaker's `ScanScript` writes them.

use om_formula::Names;
use om_scripts::{flag, Calculation, FieldSpec, ParamValue, Script, ScriptStep, StoredKey};

/// A probe script: `== Name [full access]` and its step lines.
#[derive(Clone, Debug)]
pub struct Probe {
    pub name: String,
    pub full_access: bool,
    /// (line number in the battery, step text).
    pub steps: Vec<(usize, String)>,
}

/// `@trigger <TAB> layout <TAB> event <TAB> script [<TAB> modes [<TAB>
/// parameter]]`: a layout trigger of the probe file (the layout's own
/// triggers are replaced).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerSpec {
    pub layout: String,
    /// FileMaker's XML name of the event (`OnLayoutExit`).
    pub event: String,
    pub script: String,
    pub modes: String,
    pub parameter: String,
}

impl TriggerSpec {
    /// The event id (script-runtime.md 7.1) of a layout event name.
    pub fn event_id(&self) -> u16 {
        const EVENTS: [(&str, u16); 12] = [
            ("OnLayoutEnter", 101),
            ("OnLayoutKeystroke", 102),
            ("OnRecordLoad", 103),
            ("OnRecordCommit", 104),
            ("OnRecordRevert", 105),
            ("OnModeEnter", 106),
            ("OnModeExit", 107),
            ("OnLayoutExit", 109),
            ("OnViewChange", 110),
            ("OnLayoutSizeChange", 111),
            ("OnGestureTap", 112),
            ("OnExternalCommandReceived", 113),
        ];
        EVENTS
            .iter()
            .find(|(name, _)| *name == self.event)
            .unwrap_or_else(|| panic!("unknown layout event {:?}", self.event))
            .1
    }
}

/// `@call <TAB> script <TAB> line <TAB> target <TAB> state <TAB> bottom`:
/// after that step has run, start `target` as a button or trigger does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSpec {
    pub script: String,
    pub line: usize,
    pub target: String,
    pub state: u16,
    pub bottom: bool,
}

/// One line of the run section.
#[derive(Clone, Debug)]
pub enum RunLine {
    /// Run `script` with `parameter`, then evaluate `expressions`.
    Run {
        script: String,
        parameter: String,
        expressions: Vec<String>,
    },
    /// `@call ...`; `None` (a bare `@call`) clears every call.
    Call(Option<CallSpec>),
    /// `@watch expr ...`; empty clears.
    Watch(Vec<String>),
}

#[derive(Clone, Debug, Default)]
pub struct Battery {
    pub probes: Vec<Probe>,
    pub triggers: Vec<TriggerSpec>,
    pub runs: Vec<RunLine>,
}

/// Read a battery as `runbat.sh` splits it: `== Name` blocks before
/// `--- run` are probes; the lines after it are run lines (without a run
/// section every probe runs once; a file without probes is all run lines).
pub fn parse(text: &str) -> Battery {
    let mut battery = Battery::default();
    let has_probes = text.lines().any(|l| l.starts_with("=="));
    let mut in_run = !has_probes;
    let mut current: Option<Probe> = None;
    for (n, raw) in text.lines().enumerate() {
        if in_run {
            if let Some(run) = run_line(raw) {
                battery.runs.push(run);
            }
            continue;
        }
        if raw.starts_with("--- run") {
            in_run = true;
            continue;
        }
        if raw.starts_with("@trigger") {
            let fields: Vec<&str> = raw.split('\t').collect();
            assert!(fields.len() >= 4, "line {}: {raw}", n + 1);
            let field = |i: usize| fields.get(i).map_or(String::new(), |f| (*f).to_owned());
            battery.triggers.push(TriggerSpec {
                layout: field(1),
                event: field(2),
                script: field(3),
                modes: field(4),
                parameter: field(5),
            });
            continue;
        }
        let line = raw.trim();
        if let Some(header) = line.strip_prefix("==") {
            let header = header.trim();
            let (name, full_access) = match header.strip_suffix("[full access]") {
                Some(name) => (name.trim(), true),
                None => (header, false),
            };
            battery.probes.extend(current.take());
            current = Some(Probe {
                name: name.to_owned(),
                full_access,
                steps: Vec::new(),
            });
            continue;
        }
        if let (Some(probe), false) = (&mut current, line.is_empty()) {
            probe.steps.push((n + 1, line.to_owned()));
        }
    }
    battery.probes.extend(current);
    if has_probes && !text.lines().any(|l| l.starts_with("--- run")) {
        battery.runs = battery
            .probes
            .iter()
            .map(|p| RunLine::Run {
                script: p.name.clone(),
                parameter: String::new(),
                expressions: Vec::new(),
            })
            .collect();
    }
    battery
}

fn run_line(raw: &str) -> Option<RunLine> {
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    let fields: Vec<String> = raw.split('\t').map(unescape).collect();
    match fields[0].as_str() {
        "@call" if fields.len() >= 6 => Some(RunLine::Call(Some(CallSpec {
            script: fields[1].clone(),
            line: fields[2].parse().expect("@call line"),
            target: fields[3].clone(),
            state: fields[4].parse().expect("@call state"),
            bottom: fields[5] == "1",
        }))),
        "@call" => Some(RunLine::Call(None)),
        "@watch" => Some(RunLine::Watch(fields[1..].to_vec())),
        directive if directive.starts_with('@') => {
            panic!("the oracle test does not support {directive}")
        }
        _ => Some(RunLine::Run {
            script: fields[0].clone(),
            parameter: fields.get(1).cloned().unwrap_or_default(),
            expressions: fields.get(2..).map(<[String]>::to_vec).unwrap_or_default(),
        }),
    }
}

/// The harness's escapes: `\n \r \t \\ \uXXXX`.
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.push(
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .expect("\\u escape"),
                );
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// What step lines refer to by name.
pub struct Catalog<'a> {
    pub names: &'a dyn Names,
    /// Scripts by name: the probes' and the file's.
    pub scripts: Vec<(String, u64)>,
    /// Layouts by name.
    pub layouts: Vec<(String, u64)>,
}

impl Catalog<'_> {
    fn script(&self, name: &str) -> u64 {
        self.scripts
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("unknown script {name:?}"))
            .1
    }

    fn layout(&self, name: &str) -> u64 {
        self.layouts
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("unknown layout {name:?}"))
            .1
    }

    /// A step calculation compiled as FileMaker's importer stores it. A
    /// field the importer cannot resolve (`Contacts::NoSuchField`) leaves a
    /// calculation the engine evaluates as empty (script-runtime.md 13.3),
    /// so it compiles as `""`.
    pub fn calc(&self, text: &str) -> Calculation {
        let ctx = om_formula::Context::new(self.names);
        let compiled = match om_formula::compile(text, &ctx) {
            Ok(compiled) => compiled,
            Err(e)
                if matches!(
                    e.code,
                    om_formula::ErrorCode::FieldMissing | om_formula::ErrorCode::TableMissing
                ) =>
            {
                om_formula::compile("\"\"", &ctx).expect("empty text")
            }
            Err(e) => panic!("{text}: {e:?}"),
        };
        Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )
    }

    /// `Table::Field` as a field spec of repetition 1.
    fn field(&self, reference: &str) -> FieldSpec {
        let (table, field) = reference
            .trim()
            .split_once("::")
            .unwrap_or_else(|| panic!("{reference:?} is not Table::Field"));
        let occurrence = self
            .names
            .find_occurrence(table.trim())
            .unwrap_or_else(|| panic!("unknown table occurrence {table:?}"));
        let id = self
            .names
            .find_field(occurrence, field.trim())
            .unwrap_or_else(|| panic!("unknown field {reference:?}"));
        FieldSpec::new(
            StoredKey::from_id(occurrence).expect("key"),
            StoredKey::from_id(id).expect("key"),
            1,
        )
    }

    /// One probe as a script with id `id`. Block indexes are computed as
    /// `ScanScript` writes them; for unbalanced blocks (Commit Transaction
    /// without Open Transaction, ...) ScanScript fails with 3702 and the
    /// importer's zeros stay.
    pub fn script_of(&self, probe: &Probe, id: u64) -> Script {
        let steps: Vec<ScriptStep> = probe
            .steps
            .iter()
            .map(|(n, line)| {
                self.step(line)
                    .unwrap_or_else(|e| panic!("line {n}: {line}: {e}"))
            })
            .collect();
        let mut script = Script {
            id,
            name: probe.name.clone(),
            ..Default::default()
        };
        script.set_runs_with_full_access(probe.full_access);
        if script.insert_steps(0, steps.clone()).is_err() {
            script.steps = steps;
        }
        script
    }

    /// A step line in `make_patch.py`'s syntax.
    pub fn step(&self, line: &str) -> Result<ScriptStep, String> {
        if let Some(rest) = line.strip_prefix("//") {
            let mut step = self.step(rest.trim())?;
            step.set_enabled(false);
            return Ok(step);
        }
        if let Some(text) = line.strip_prefix('#') {
            let mut step = ScriptStep::new(89);
            step.set_text(0, Some(text.trim()));
            return Ok(step);
        }
        if let Some(id) = line.strip_prefix("Step ") {
            return Ok(ScriptStep::new(id.trim().parse().map_err(|_| "bad id")?));
        }
        let (name, mut args) = match line.find('[') {
            Some(open) if line.ends_with(']') => (
                line[..open].trim(),
                split_args(&line[open + 1..line.len() - 1]),
            ),
            _ => (line.trim(), Vec::new()),
        };
        let id = step_id(name).ok_or_else(|| format!("unknown step {name:?}"))?;
        let mut step = ScriptStep::new(id);
        match id {
            // Perform Script, Perform Script on Server
            1 | 164 => {
                let wait = keyword(&mut args, "Wait").map(|w| on_off(&w));
                if !args.is_empty() {
                    let target = args.remove(0);
                    self.script_target(&mut step, &target)?;
                }
                let parameter = keyword(&mut args, "Parameter")
                    .or_else(|| (!args.is_empty()).then(|| args.remove(0)));
                if let Some(parameter) = parameter.filter(|p| !p.is_empty()) {
                    step.set_calculation(0, Some(&self.calc(&parameter)));
                }
                if id == 164 {
                    step.set_flag(flag::OPTION, wait == Some(false));
                }
            }
            // Go to Layout
            6 => match args.first().map(|a| a.trim()) {
                None | Some("original") => step.value = 1,
                Some(layout) => {
                    step.value = 5;
                    step.set_member_key(StoredKey::from_id(self.layout(&unquote(layout)?)));
                }
            },
            // Delete Record/Request, Commit Records/Requests
            9 | 75 => {
                let dialog = keyword(&mut args, "With dialog").is_some_and(|d| on_off(&d));
                step.set_parameter("With dialog", ParamValue::Bool(dialog))
                    .map_err(|e| e.0)?;
                if id == 75
                    && args
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case("skip validation"))
                {
                    step.set_flag(flag::OPTION, true);
                }
            }
            // Go to Record/Request/Page
            16 => {
                let exit = keyword(&mut args, "Exit after last").is_some_and(|e| on_off(&e));
                let choice = match args.first().map(|a| a.trim().to_lowercase()).as_deref() {
                    Some("first") => 1,
                    Some("last") => 2,
                    Some("previous") => 3,
                    Some("next") => 4,
                    other => return Err(format!("Go to Record {other:?}")),
                };
                step.set_parameter("Record/Request/Page", ParamValue::Choice(choice))
                    .map_err(|e| e.0)?;
                step.set_flag(flag::EXIT, exit);
            }
            // Pause/Resume Script
            62 => {
                if let Some(duration) = keyword(&mut args, "Duration") {
                    step.set_parameter("Pause", ParamValue::Choice(1))
                        .map_err(|e| e.0)?;
                    step.set_calculation(0, Some(&self.calc(&duration)));
                }
            }
            // If, Else If, Exit Loop If
            68 | 72 | 125 => step.set_calculation(0, Some(&self.calc(&arg(&args, 0)?))),
            // Loop: FileMaker's importer sets kOption on every Loop
            // (script-runtime.md 13.4).
            71 => {
                let flush = keyword(&mut args, "Flush").unwrap_or_else(|| "Always".into());
                let value = match flush.to_lowercase().as_str() {
                    "always" => 1,
                    "minimum" => 2,
                    "defer" => 3,
                    other => return Err(format!("Flush {other:?}")),
                };
                step.set_flag(flag::OPTION, true);
                step.set_list_value(1, Some(value));
            }
            // Set Field
            76 => {
                step.set_parameter("Field", ParamValue::Field(self.field(&arg(&args, 0)?)))
                    .map_err(|e| e.0)?;
                step.set_calculation(0, Some(&self.calc(&arg(&args, 1)?)));
            }
            // Allow User Abort, Set Error Capture, Set Revert Transaction on Error
            85 | 86 | 223 => step.set_flag(flag::TURN_ON, on_off(&arg(&args, 0)?)),
            // Show Custom Dialog
            87 => {
                step.set_calculation(0, Some(&self.calc(&arg(&args, 0)?)));
                if let Some(message) = args.get(1) {
                    step.set_calculation(1, Some(&self.calc(message)));
                }
                step.set_text(0, Some("OK"));
                step.set_number_bool(3, Some(true));
            }
            // Exit Script
            103 => {
                if let Some(result) = args.first().filter(|r| !r.is_empty()) {
                    step.set_calculation(0, Some(&self.calc(result)));
                }
            }
            // Set Variable [ $name[repetition] ; value ]
            141 => {
                let target = arg(&args, 0)?;
                let (name, repetition) = match target.find('[') {
                    Some(open) if target.ends_with(']') => (
                        target[..open].trim().to_owned(),
                        Some(target[open + 1..target.len() - 1].to_owned()),
                    ),
                    _ => (target.trim().to_owned(), None),
                };
                let mut rest = args[1..].to_vec();
                let value = keyword(&mut rest, "Value")
                    .or_else(|| rest.first().cloned())
                    .unwrap_or_default();
                step.set_text(0, Some(&name));
                step.set_calculation(1, Some(&self.calc(&value)));
                if let Some(repetition) = repetition {
                    step.set_calculation(2, Some(&self.calc(&repetition)));
                }
            }
            // Open Transaction
            205 => {
                for a in &args {
                    match a.to_lowercase().as_str() {
                        "skip validation" => step.set_flag(flag::OPTION, true),
                        "skip auto-enter" => step.set_flag(flag::SELECT_ALL, true),
                        "override ess" => step.set_flag(flag::OPTION2, true),
                        other => return Err(format!("Open Transaction option {other:?}")),
                    }
                }
            }
            // Revert Transaction
            207 => {
                let condition = keyword(&mut args, "Condition");
                let code = keyword(&mut args, "Error");
                let message = keyword(&mut args, "Message");
                if let Some(condition) = condition {
                    step.set_flag(flag::OPTION, true);
                    step.set_calculation(0, Some(&self.calc(&condition)));
                }
                if let Some(code) = code {
                    step.set_flag(flag::OPTION2, true);
                    step.set_calculation(1, Some(&self.calc(&code)));
                    step.set_calculation(2, Some(&self.calc(message.as_deref().unwrap_or("\"\""))));
                }
            }
            _ => {}
        }
        Ok(step)
    }

    /// `"Name"` (from the list) or `By name: calc`.
    fn script_target(&self, step: &mut ScriptStep, target: &str) -> Result<(), String> {
        let lower = target.trim_start().to_lowercase();
        if lower.starts_with("by name") {
            let calc = target
                .split_once(':')
                .ok_or("By name needs a calculation")?
                .1;
            // The corpus form: list value 1, the name calculation in slot 2.
            step.set_list_value(1, Some(1));
            step.set_calculation(2, Some(&self.calc(calc.trim())));
        } else {
            let id = self.script(&unquote(target)?);
            step.set_script_key(StoredKey::from_serial_id(id));
        }
        Ok(())
    }
}

/// `make_patch.py`'s step names.
fn step_id(name: &str) -> Option<u16> {
    Some(match name {
        "Perform Script" => 1,
        "Go to Layout" => 6,
        "New Record/Request" => 7,
        "Delete Record/Request" => 9,
        "Go to Record/Request/Page" => 16,
        "Pause/Resume Script" => 62,
        "If" => 68,
        "Else" => 69,
        "End If" => 70,
        "Loop" => 71,
        "Exit Loop If" => 72,
        "End Loop" => 73,
        "Commit Records/Requests" => 75,
        "Set Field" => 76,
        "Freeze Window" => 79,
        "Allow User Abort" => 85,
        "Set Error Capture" => 86,
        "Show Custom Dialog" => 87,
        "Halt Script" => 90,
        "Beep" => 93,
        "Exit Script" => 103,
        "Else If" => 125,
        "Set Variable" => 141,
        "Perform Script on Server" => 164,
        "Open Transaction" => 205,
        "Commit Transaction" => 206,
        "Revert Transaction" => 207,
        "Set Revert Transaction on Error" => 223,
        _ => return None,
    })
}

/// Split at `;` outside quotes, brackets and comments (`split_args`).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if quoted {
            current.push(c);
            if c == '\\' && i + 1 < chars.len() {
                current.push(chars[i + 1]);
                i += 1;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            current.push(c);
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            current.extend(&chars[i..end]);
            i = end;
            continue;
        } else if "([{".contains(c) {
            depth += 1;
            current.push(c);
        } else if ")]}".contains(c) {
            depth -= 1;
            current.push(c);
        } else if c == ';' && depth == 0 {
            out.push(current.trim().to_owned());
            current.clear();
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.trim().is_empty() || !out.is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// Remove `Key: value` from the arguments (case-insensitive).
fn keyword(args: &mut Vec<String>, key: &str) -> Option<String> {
    let i = args.iter().position(|a| {
        a.split_once(':').is_some_and(|(k, _)| {
            k.trim().eq_ignore_ascii_case(key) && !a.trim_start().starts_with('"')
        })
    })?;
    let a = args.remove(i);
    Some(a.split_once(':').expect("colon").1.trim().to_owned())
}

fn arg(args: &[String], n: usize) -> Result<String, String> {
    args.get(n)
        .cloned()
        .ok_or_else(|| format!("argument {n} missing"))
}

fn on_off(a: &str) -> bool {
    match a.trim().to_lowercase().as_str() {
        "on" | "true" => true,
        "off" | "false" => false,
        other => panic!("expected On or Off, got {other:?}"),
    }
}

fn unquote(a: &str) -> Result<String, String> {
    let a = a.trim();
    a.strip_prefix('"')
        .and_then(|a| a.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or_else(|| format!("expected a quoted name, got {a:?}"))
}
